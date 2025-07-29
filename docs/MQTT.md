# MQTT Certificate Management and TLS Implementation

## Overview

This Rust application implements secure MQTT communication with AWS IoT Core using X.509 certificate-based mutual TLS authentication. The system provides enterprise-grade security through certificate storage, validation, and encrypted communication channels.

## Table of Contents

1. [Certificate Storage Architecture](#certificate-storage-architecture)
2. [X.509 Certificate Handling](#x509-certificate-handling)
3. [TLS Mutual Authentication](#tls-mutual-authentication)
4. [MQTT Client Implementation](#mqtt-client-implementation)
5. [AWS IoT Core Integration](#aws-iot-core-integration)
6. [Security Implementation](#security-implementation)
7. [Memory Optimization](#memory-optimization)
8. [Error Handling and Validation](#error-handling-and-validation)

## Certificate Storage Architecture

### NVS Flash Storage

The application uses ESP-IDF's Non-Volatile Storage (NVS) system to securely store AWS IoT Core certificates in flash memory. This provides persistent storage that survives device reboots and power cycles.

#### Storage Structure

```rust
// NVS namespace and keys for certificate management
const NVS_NAMESPACE: &str = "mqtt_certs";
const DEVICE_CERT_KEY: &str = "device_cert";     // Device certificate (X.509 PEM)
const PRIVATE_KEY_KEY: &str = "private_key";     // Device private key (RSA PEM)
const IOT_ENDPOINT_KEY: &str = "iot_endpoint";   // AWS IoT Core endpoint URL
const CERT_METADATA_KEY: &str = "cert_meta";     // Certificate metadata (JSON)
const CERT_VALIDATION_KEY: &str = "cert_valid";  // Validation status flag
```

#### Certificate Metadata

Each certificate set includes comprehensive metadata stored as JSON:

```rust
pub struct CertificateMetadata {
    pub stored_at: u64,                    // Unix timestamp when stored
    pub device_id: String,                 // Associated device ID
    pub certificate_fingerprint: String,   // SHA256 fingerprint for validation
    pub iot_endpoint: String,              // AWS IoT endpoint for quick access
    pub is_valid: bool,                    // Validation status
    pub last_used: Option<u64>,            // Last usage timestamp
    pub validation_attempts: u32,          // Number of validation attempts
}
```

### Dynamic Memory Optimization

The system implements intelligent buffer sizing to optimize ESP32 memory usage:

#### Standard vs. Optimized Loading

**Standard Loading (Fixed Buffers):**
```rust
// Fixed 2KB buffers (wasteful)
let mut cert_buffer = [0u8; 2048];
let mut key_buffer = [0u8; 2048]; 
let mut endpoint_buffer = [0u8; 256];
// Total: 4352 bytes allocated
```

**Optimized Loading (Dynamic Buffers):**
```rust
// Dynamic sizing based on actual content
let cert_size = std::cmp::max(self.get_certificate_size(DEVICE_CERT_KEY), 1500);
let key_size = std::cmp::max(self.get_certificate_size(PRIVATE_KEY_KEY), 1700);
let endpoint_size = std::cmp::max(self.get_certificate_size(IOT_ENDPOINT_KEY), 256);

let mut cert_buffer = vec![0u8; cert_size + 1];      // ~1500 bytes typical
let mut key_buffer = vec![0u8; key_size + 1];        // ~1700 bytes typical  
let mut endpoint_buffer = vec![0u8; endpoint_size + 1]; // ~256 bytes typical
// Total: ~3456 bytes allocated (600+ bytes saved)
```

## X.509 Certificate Handling

### Certificate Format Conversion

The application converts PEM-formatted certificates to ESP-IDF's required X.509 format:

```rust
fn convert_pem_to_x509(pem_cert: &str) -> Result<X509<'static>> {
    use std::ffi::CString;

    // Convert PEM string to CString (adds null terminator automatically)
    let c_string = CString::new(pem_cert)
        .map_err(|e| anyhow!("PEM certificate contains null bytes: {}", e))?;

    // Create static lifetime CStr using Box::leak()
    let static_cstr: &'static std::ffi::CStr = Box::leak(c_string.into_boxed_c_str());

    // Create X.509 certificate from static CStr
    Ok(X509::pem(static_cstr))
}
```

### Certificate Components

The system handles three certificate components:

1. **Device Certificate**: Unique X.509 certificate identifying the specific ESP32 device
2. **Private Key**: RSA private key corresponding to the device certificate
3. **Amazon Root CA 1**: Root certificate for validating AWS IoT Core server certificates

### Validation Process

Comprehensive certificate validation includes:

```rust
fn validate_certificate_format(&self, certificates: &DeviceCertificates) -> Result<(), CertificateValidation> {
    // Validate device certificate PEM format
    if !certificates.device_certificate.starts_with("-----BEGIN CERTIFICATE-----") ||
       !certificates.device_certificate.ends_with("-----END CERTIFICATE-----") {
        return Err(CertificateValidation::InvalidFormat);
    }

    // Validate private key PEM format  
    if !certificates.private_key.starts_with("-----BEGIN PRIVATE KEY-----") ||
       !certificates.private_key.ends_with("-----END PRIVATE KEY-----") {
        return Err(CertificateValidation::InvalidFormat);
    }

    // Validate IoT endpoint format (AWS hostname structure)
    if !certificates.iot_endpoint.contains(".iot.") ||
       !certificates.iot_endpoint.contains(".amazonaws.com") {
        return Err(CertificateValidation::InvalidContent);
    }

    // Check minimum size requirements
    if certificates.device_certificate.len() < 500 || certificates.private_key.len() < 500 {
        return Err(CertificateValidation::InvalidContent);
    }

    Ok(())
}
```

## TLS Mutual Authentication

### Mutual TLS (mTLS) Configuration

The application implements full mutual TLS authentication where both client and server verify each other's identity:

```rust
// Convert certificates to X.509 format for ESP-IDF
let device_cert = Self::convert_pem_to_x509(&certificates.device_certificate)?;
let private_key = Self::convert_pem_to_x509(&certificates.private_key)?;
let aws_root_ca = Self::convert_pem_to_x509(AWS_ROOT_CA_1)?;

// MQTT client configuration with X.509 certificate authentication
let mqtt_config = MqttClientConfiguration {
    client_id: Some(&self.client_id),
    keep_alive_interval: Some(std::time::Duration::from_secs(60)),
    reconnect_timeout: Some(std::time::Duration::from_secs(30)),
    network_timeout: std::time::Duration::from_secs(30),

    // X.509 certificate configuration for mutual TLS authentication
    client_certificate: Some(device_cert),      // Device identity
    private_key: Some(private_key),             // Device private key
    server_certificate: Some(aws_root_ca),      // AWS IoT Core validation
    use_global_ca_store: false,                 // Use specific AWS Root CA
    skip_cert_common_name_check: false,         // Verify server identity

    ..Default::default()
};
```

### TLS Handshake Process

1. **Client Hello**: ESP32 initiates connection to AWS IoT Core (port 8883)
2. **Server Certificate**: AWS IoT Core presents its certificate
3. **Certificate Verification**: ESP32 validates server certificate against Amazon Root CA 1
4. **Client Certificate Request**: AWS IoT Core requests client certificate
5. **Client Certificate**: ESP32 presents device certificate and private key proof
6. **Mutual Verification**: Both parties verify certificate validity and authorization
7. **Secure Channel**: Encrypted TLS 1.2+ tunnel established for MQTT communication

### Security Features

- **TLS 1.2+ Encryption**: All MQTT traffic encrypted using industry-standard protocols
- **Certificate Pinning**: Server validation against specific Amazon Root CA 1
- **Client Authentication**: Device identity verified through X.509 certificates
- **Perfect Forward Secrecy**: Session keys generated for each connection
- **Certificate Revocation**: Support for certificate lifecycle management

## MQTT Client Implementation

### Connection Management

```rust
pub async fn connect(&mut self) -> Result<()> {
    // Validate certificates are available
    if self.certificates.is_none() {
        return Err(anyhow!("No X.509 certificates available for MQTT connection"));
    }

    let certificates = self.certificates.as_ref().unwrap();
    self.connection_status = ConnectionStatus::Connecting;
    
    // Create secure MQTT broker URL
    let broker_url = format!("mqtts://{}:8883", certificates.iot_endpoint);
    
    // Convert certificates and establish connection
    let device_cert = Self::convert_pem_to_x509(&certificates.device_certificate)?;
    let private_key = Self::convert_pem_to_x509(&certificates.private_key)?;
    let aws_root_ca = Self::convert_pem_to_x509(AWS_ROOT_CA_1)?;

    let mqtt_config = MqttClientConfiguration {
        // ... certificate configuration
    };

    // Create ESP MQTT client with X.509 certificate-based TLS authentication
    match EspMqttClient::new(&broker_url, &mqtt_config) {
        Ok((mut client, _connection)) => {
            self.subscribe_to_device_topics(&mut client).await?;
            self.client = Some(client);
            self.connection_status = ConnectionStatus::Connected;
            Ok(())
        }
        Err(e) => {
            self.connection_status = ConnectionStatus::Error(error_msg.clone());
            Err(anyhow!(error_msg))
        }
    }
}
```

### Topic Structure

The application uses a structured topic hierarchy for AWS IoT Core communication:

```rust
// MQTT topic patterns following AWS IoT Core conventions
const TOPIC_BUTTON_PRESS: &str = "acorn-pups/button-press";
const TOPIC_DEVICE_STATUS: &str = "acorn-pups/status";
const TOPIC_SETTINGS: &str = "acorn-pups/settings";
const TOPIC_COMMANDS: &str = "acorn-pups/commands";
```

**Device-Specific Topics:**
- `acorn-pups/button-press/{device_id}` - Button press events
- `acorn-pups/status/{device_id}` - Device status updates
- `acorn-pups/settings/{device_id}` - Configuration updates (subscribed)
- `acorn-pups/commands/{device_id}` - Remote commands (subscribed)

### Message Formats

All messages use JSON serialization with consistent field naming:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonPressMessage {
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "buttonRfId")]
    pub button_rf_id: String,
    pub timestamp: String,                    // ISO 8601 format
    #[serde(rename = "batteryLevel")]
    pub battery_level: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatusMessage {
    #[serde(rename = "deviceId")]
    pub device_id: String,
    pub timestamp: String,
    pub status: String,                       // "online", "offline", "error"
    #[serde(rename = "firmwareVersion")]
    pub firmware_version: String,
}
```

## AWS IoT Core Integration

### Device Registration Flow

1. **WiFi Connection**: Device connects to internet via stored WiFi credentials
2. **HTTP Registration**: Device calls backend API to register and receive certificates
3. **Certificate Storage**: Certificates stored securely in NVS flash
4. **MQTT Initialization**: MQTT client loads certificates and establishes secure connection
5. **Operational Mode**: Device begins publishing sensor data and receiving commands

### AWS IoT Core Configuration

The system connects to AWS IoT Core using:

- **Endpoint**: Device-specific AWS IoT Core endpoint (e.g., `abc123-ats.iot.us-west-2.amazonaws.com`)
- **Port**: 8883 (MQTT over TLS)
- **Protocol**: MQTT 3.1.1 with TLS 1.2+ encryption
- **Authentication**: X.509 client certificates with mutual TLS
- **QoS**: At Least Once (QoS 1) for reliable message delivery

### Connection Resilience

```rust
// Connection retry configuration with exponential backoff
const INITIAL_RETRY_DELAY_MS: u64 = 1000;    // 1 second
const MAX_RETRY_DELAY_MS: u64 = 60000;       // 60 seconds  
const MAX_RETRY_ATTEMPTS: u32 = 10;

pub async fn attempt_reconnection(&mut self) -> Result<()> {
    if self.retry_count >= MAX_RETRY_ATTEMPTS {
        return Err(anyhow!("Maximum retry attempts exceeded"));
    }

    // Exponential backoff with maximum delay
    self.retry_delay_ms = std::cmp::min(self.retry_delay_ms * 2, MAX_RETRY_DELAY_MS);
    
    // Wait before retry attempt
    Timer::after(Duration::from_millis(self.retry_delay_ms)).await;
    
    // Attempt reconnection with full X.509 authentication
    self.connect().await
}
```

## Security Implementation

### Certificate Lifecycle

1. **Generation**: AWS IoT Core generates unique device certificates
2. **Provisioning**: Certificates delivered via secure device registration API
3. **Storage**: Certificates stored encrypted in ESP32 NVS flash
4. **Validation**: Comprehensive format and content validation before use
5. **Usage**: Certificates used for all MQTT connections with AWS IoT Core
6. **Rotation**: Support for certificate updates through device management

### Security Best Practices

- **Certificate Isolation**: Each device has unique certificates
- **Private Key Protection**: Private keys never transmitted or logged
- **Endpoint Validation**: Server certificate validation against Amazon Root CA
- **Connection Encryption**: All traffic encrypted with TLS 1.2+
- **Certificate Fingerprinting**: SHA256 fingerprints for certificate integrity
- **Access Control**: AWS IoT policies restrict device permissions

### Attack Mitigation

- **Man-in-the-Middle**: Prevented by mutual certificate validation
- **Replay Attacks**: Mitigated by TLS session encryption and nonces
- **Certificate Theft**: Private keys remain on device, never transmitted
- **Unauthorized Access**: AWS IoT policies enforce device-specific permissions
- **Data Tampering**: TLS encryption prevents message modification

## Memory Optimization

### Buffer Size Optimization

The optimized certificate loading provides significant memory savings:

```rust
// Memory usage comparison
Traditional Fixed Buffers:
- Device Certificate: 2048 bytes
- Private Key: 2048 bytes  
- IoT Endpoint: 256 bytes
- Total: 4352 bytes

Optimized Dynamic Buffers:
- Device Certificate: ~1500 bytes (actual size + 1)
- Private Key: ~1700 bytes (actual size + 1)
- IoT Endpoint: ~256 bytes (actual size + 1)  
- Total: ~3456 bytes
- Memory Saved: ~896 bytes (20.6% reduction)
```

### Stack Usage

- **Certificate Loading**: Dynamic allocation on heap (not stack)
- **X.509 Conversion**: Static lifetime management via `Box::leak()`
- **MQTT Configuration**: Minimal stack usage with reference passing
- **Connection State**: Efficient enum-based state management

## Error Handling and Validation

### Certificate Validation Errors

```rust
pub enum CertificateValidation {
    Valid,
    InvalidFormat,      // PEM format validation failed
    InvalidContent,     // Content validation failed  
    Expired,           // Certificate past expiration
    Corrupted,         // Certificate data corrupted
    Missing,           // Certificate not found
}
```

### MQTT Connection Errors

```rust
pub enum ConnectionStatus {
    Disconnected,
    Connecting, 
    Connected,
    Error(String),     // Detailed error information
}
```

### Error Recovery

- **Certificate Errors**: Graceful fallback with detailed error reporting
- **Connection Failures**: Exponential backoff retry with maximum attempts
- **TLS Handshake Failures**: Certificate validation error reporting
- **Network Issues**: Automatic reconnection with connection health monitoring
- **Message Failures**: Queue-based retry mechanism with timeout handling

## Monitoring and Diagnostics

### Connection Health Monitoring

```rust
// Regular connection health checks
const CONNECTION_CHECK_INTERVAL_SECONDS: u64 = 30;

async fn check_connection_health(&mut self) {
    match self.client.get_connection_status() {
        ConnectionStatus::Connected => {
            // Process pending messages
            self.client.process_messages().await;
        }
        ConnectionStatus::Disconnected => {
            // Attempt automatic reconnection
            self.attempt_reconnection().await;
        }
        ConnectionStatus::Error(ref error) => {
            // Log error and attempt recovery
            self.force_reconnection().await;
        }
    }
}
```

### Logging and Diagnostics

- **Certificate Loading**: Detailed logs for storage and validation steps
- **TLS Handshake**: Progress logging for connection establishment  
- **MQTT Operations**: Message publishing and subscription logging
- **Error Tracking**: Comprehensive error logging with context
- **Performance Metrics**: Memory usage and connection timing statistics

## Conclusion

This implementation provides enterprise-grade security for IoT device communication through:

- **Robust Certificate Management**: Secure storage and validation of X.509 certificates
- **Mutual TLS Authentication**: Full bidirectional certificate validation
- **AWS IoT Core Integration**: Native support for AWS IoT Core services
- **Memory Optimization**: Efficient memory usage for resource-constrained devices
- **Error Resilience**: Comprehensive error handling and automatic recovery
- **Security Best Practices**: Industry-standard security implementations

The system ensures secure, reliable, and efficient MQTT communication while maintaining the strict security requirements for IoT device deployments. 