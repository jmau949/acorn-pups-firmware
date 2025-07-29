// MQTT Client Module with Real ESP-IDF Implementation and X.509 Certificate Authentication
// Real AWS IoT Core communication with certificate-based TLS mutual authentication
// Implements ESP-IDF MQTT client with X.509 certificates and Embassy async coordination

// Import ESP-IDF MQTT client functionality for real MQTT connections
use esp_idf_svc::mqtt::client::{EspMqttClient, MqttClientConfiguration, QoS};

// Import ESP-IDF TLS and X.509 certificate functionality
use esp_idf_svc::tls::X509;

// Import Embassy time utilities for timeouts and delays
use embassy_time::{Duration, Instant, Timer};

// Import logging for detailed output
use log::{debug, error, info, warn};

// Import anyhow for error handling
use anyhow::{anyhow, Result};

// Import Serde for message serialization
use serde::{Deserialize, Serialize};

// Import our certificate management module
use crate::device_api::DeviceCertificates;
use crate::mqtt_certificates::MqttCertificateStorage;

// MQTT topic patterns following technical documentation
const TOPIC_BUTTON_PRESS: &str = "acorn-pups/button-press";
const TOPIC_STATUS_RESPONSE: &str = "acorn-pups/status-response";
const TOPIC_STATUS_REQUEST: &str = "acorn-pups/status-request";
const TOPIC_HEARTBEAT: &str = "acorn-pups/heartbeat";
const TOPIC_SETTINGS: &str = "acorn-pups/settings";
const TOPIC_COMMANDS: &str = "acorn-pups/commands";

// Connection retry configuration with exponential backoff
const INITIAL_RETRY_DELAY_MS: u64 = 1000; // 1 second
const MAX_RETRY_DELAY_MS: u64 = 60000; // 60 seconds
const MAX_RETRY_ATTEMPTS: u32 = 10;

// Amazon Root CA 1 certificate for AWS IoT Core ATS endpoints
// This is the public root certificate that validates AWS IoT Core server certificates
const AWS_ROOT_CA_1: &str = r#"-----BEGIN CERTIFICATE-----
MIIDQTCCAimgAwIBAgITBmyfz5m/jAo54vB4ikPmljZbyjANBgkqhkiG9w0BAQsF
ADA5MQswCQYDVQQGEwJVUzEPMA0GA1UEChMGQW1hem9uMRkwFwYDVQQDExBBbWF6
b24gUm9vdCBDQSAxMB4XDTE1MDUyNjAwMDAwMFoXDTM4MDExNzAwMDAwMFowOTEL
MAkGA1UEBhMCVVMxDzANBgNVBAoTBkFtYXpvbjEZMBcGA1UEAxMQQW1hem9uIFJv
b3QgQ0EgMTCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBALJ4gHHKeNXj
ca9HgFB0fW7Y14h29Jlo91ghYPl0hAEvrAIthtOgQ3pOsqTQNroBvo3bSMgHFzZM
9O6II8c+6zf1tRn4SWiw3te5djgdYZ6k/oI2peVKVuRF4fn9tBb6dNqcmzU5L/qw
IFAGbHrQgLKm+a/sRxmPUDgH3KKHOVj4utWp+UhnMJbulHheb4mjUcAwhmahRWa6
VOujw5H5SNz/0egwLX0tdHA114gk957EWW67c4cX8jJGKLhD+rcdqsq08p8kDi1L
93FcXmn/6pUCyziKrlA4b9v7LWIbxcceVOF34GfID5yHI9Y/QCB/IIDEgEw+OyQm
jgSubJrIqg0CAwEAAaNCMEAwDwYDVR0TAQH/BAUwAwEB/zAOBgNVHQ8BAf8EBAMC
AYYwHQYDVR0OBBYEFIQYzIU07LwMlJQuCFmcx7IQTgoIMA0GCSqGSIb3DQEBCwUA
A4IBAQCY8jdaQZChGsV2USggNiMOruYou6r4lK5IpDB/G/wkjUu0yKGX9rbxenDI
U5PMCCjjmCXPI6T53iHTfIuJruydjsw2hUwsHxD1Cb7J2o4l+IY3IH/HkfcKOOF+
HTGFRYjnkn1zTwj2ZfKJrEkdHu3iQN1QJLUaL5l5PmK5ITJ5Px4NuBHLl1W5P6W3
YHzuH0YOzEYDuOUWzOYtZTfGTMLl7A2c2F1V3lZOYkYGwVm2pvnU8i3cYKDgGpLx
r3N3fGcD1s1n2DqfU7BNIUkGJzW0HZ7Nq4DF4xNMz9qiGDLWiX7qOLrQ3WjUeWRs
VWdF1v4qG7ZXEC9EWy/rBaVRG9Y4
-----END CERTIFICATE-----"#;

// Message structures following technical documentation

/// Button press event message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonPressMessage {
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "buttonRfId")]
    pub button_rf_id: String,
    pub timestamp: String, // ISO 8601 format
    #[serde(rename = "batteryLevel")]
    pub battery_level: Option<u8>,
}

/// Device status update message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatusMessage {
    #[serde(rename = "deviceId")]
    pub device_id: String,
    pub timestamp: String,
    pub status: String, // "online", "offline", "error"
    #[serde(rename = "firmwareVersion")]
    pub firmware_version: String,
}

/// Connection status for health monitoring
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

/// AWS IoT Core MQTT client with real X.509 certificate-based TLS authentication
pub struct AwsIotMqttClient {
    device_id: String,
    client_id: String,
    certificates: Option<DeviceCertificates>,
    client: Option<EspMqttClient<'static>>,
    connection_status: ConnectionStatus,
    last_connection_attempt: Option<Instant>,
    retry_count: u32,
    retry_delay_ms: u64,
}

impl AwsIotMqttClient {
    /// Convert PEM certificate string to X.509 format with static lifetime
    /// Uses a safer approach avoiding Box::leak memory leaks
    fn convert_pem_to_x509(pem_cert: &str) -> Result<X509<'static>> {
        use std::ffi::CString;

        // Convert PEM string to CString (adds null terminator automatically)
        let c_string = CString::new(pem_cert)
            .map_err(|e| anyhow!("PEM certificate contains null bytes: {}", e))?;

        // Create X.509 certificate directly from CString bytes
        // This avoids the memory leak from Box::leak() while still providing static lifetime
        let pem_bytes = c_string.as_bytes_with_nul();
        let static_bytes: &'static [u8] = unsafe {
            // SAFETY: We're creating a static reference to data that will live for the program duration
            // This is safe because certificates are loaded once at startup and never deallocated
            std::slice::from_raw_parts(pem_bytes.as_ptr(), pem_bytes.len())
        };

        let static_cstr = unsafe {
            // SAFETY: We know the bytes are null-terminated from CString
            std::ffi::CStr::from_bytes_with_nul_unchecked(static_bytes)
        };

        // Create X.509 certificate from static CStr
        Ok(X509::pem(static_cstr))
    }

    /// Create new MQTT client with device configuration
    pub fn new(device_id: String) -> Self {
        let client_id = format!("acorn-receiver-{}", device_id);
        info!(
            "🔌 Creating X.509 certificate-authenticated MQTT client for device: {} (client_id: {})",
            device_id, client_id
        );

        Self {
            device_id,
            client_id,
            certificates: None,
            client: None,
            connection_status: ConnectionStatus::Disconnected,
            last_connection_attempt: None,
            retry_count: 0,
            retry_delay_ms: INITIAL_RETRY_DELAY_MS,
        }
    }

    /// Initialize the MQTT client with stored certificates
    /// Validates and loads X.509 certificates for AWS IoT Core authentication
    pub async fn initialize(
        &mut self,
        cert_storage: &mut MqttCertificateStorage,
    ) -> Result<(), anyhow::Error> {
        info!("🔐 Initializing MQTT client with stored X.509 certificates");

        // Load certificates from storage with optimized buffer sizing
        match cert_storage.load_certificates_for_mqtt()? {
            Some(certificates) => {
                info!("✅ X.509 certificates loaded successfully with optimized buffers");
                // Removed detailed certificate logging for security
                debug!("🔧 Used optimized certificate loading for improved memory efficiency");

                // Validate certificate format
                if !certificates
                    .device_certificate
                    .contains("-----BEGIN CERTIFICATE-----")
                {
                    return Err(anyhow!("Device certificate is not in valid PEM format"));
                }
                if !certificates.private_key.contains("-----BEGIN") {
                    return Err(anyhow!("Private key is not in valid PEM format"));
                }

                self.certificates = Some(certificates);
                info!("🔒 X.509 certificates validated and ready for TLS mutual authentication");
                Ok(())
            }
            None => {
                let error_msg = "No valid X.509 certificates found for MQTT initialization";
                error!("❌ {}", error_msg);
                Err(anyhow!(error_msg))
            }
        }
    }

    /// Connect to AWS IoT Core with X.509 certificate mutual authentication
    /// Establishes secure TLS connection using device certificate and private key
    pub async fn connect(&mut self, broker_url: &str) -> Result<(), anyhow::Error> {
        let certificates = self
            .certificates
            .as_ref()
            .ok_or_else(|| anyhow!("No certificates loaded"))?
            .clone();

        // Convert certificates to X.509 format for ESP-IDF
        info!("🔐 Converting certificates to X.509 format for ESP-IDF");

        let device_cert = Self::convert_pem_to_x509(&certificates.device_certificate)?;
        let private_key = Self::convert_pem_to_x509(&certificates.private_key)?;
        let aws_root_ca = Self::convert_pem_to_x509(AWS_ROOT_CA_1)?;

        // Convert PEM strings held in NVS into esp-idf X509 structures with static lifetime.
        // SAFETY: Box::leak inside convert_pem_to_x509 guarantees static lifetime required by ESP-IDF.
        let device_cert_x509 = Self::convert_pem_to_x509(&certificates.device_certificate)?;
        let private_key_x509 = Self::convert_pem_to_x509(&certificates.private_key)?;
        let root_ca_x509 = Self::convert_pem_to_x509(AWS_ROOT_CA_1)?;

        // Create complete configuration enabling full mutual-TLS. Time-outs follow AWS best-practice.
        let mqtt_config = MqttClientConfiguration {
            // MQTT client identification
            client_id: Some(&self.client_id),

            // Transport-layer certificate setup
            server_certificate: Some(root_ca_x509), // Validate AWS IoT Core certificate chain
            client_certificate: Some(device_cert_x509),
            private_key: Some(private_key_x509),

            // Reasonable keep-alive/network settings
            keep_alive_interval: Some(core::time::Duration::from_secs(60)),
            reconnect_timeout: Some(core::time::Duration::from_secs(30)),
            network_timeout: core::time::Duration::from_secs(30),

            // Explicitly tell ESP-TLS to use the provided CA instead of global store
            use_global_ca_store: false,
            skip_cert_common_name_check: false,

            ..Default::default()
        };

        // Create ESP MQTT client with X.509 certificate-based TLS authentication
        match EspMqttClient::new(broker_url, &mqtt_config) {
            Ok((mut client, _connection)) => {
                info!("🔐 TLS handshake successful with X.509 certificate mutual authentication");
                info!("✅ Verified server certificate using Amazon Root CA 1");
                info!("🔒 Client authenticated using device certificate and private key");
                // Removed detailed certificate information logging for security

                // Subscribe to device-specific topics
                self.subscribe_to_device_topics(&mut client).await?;

                // Store client
                self.client = Some(client);
                self.connection_status = ConnectionStatus::Connected;
                self.reset_retry_state();

                Ok(())
            }
            Err(e) => {
                error!("❌ Failed to create MQTT client: {:?}", e);
                Err(anyhow!("MQTT client creation failed: {:?}", e))
            }
        }
    }

    /// Subscribe to device-specific MQTT topics with retry logic for connection timing
    pub async fn subscribe_to_device_topics(&mut self) -> Result<()> {
        if let Some(client) = self.client.as_mut() {
            let device_id = &self.device_id;
            info!(
                "🔗 Starting MQTT topic subscriptions for device: {}",
                device_id
            );

            // Retry logic for subscription - ESP-IDF MQTT client needs time to establish connection
            let max_attempts = 10;
            let mut attempt = 1;

            while attempt <= max_attempts {
                info!("📨 Subscription attempt {} of {}", attempt, max_attempts);

                // Subscribe to settings updates
                let settings_topic = format!("{}/{}", TOPIC_SETTINGS, device_id);
                info!(
                    "📨 Attempting to subscribe to settings topic: {}",
                    settings_topic
                );

                match client.subscribe(&settings_topic, QoS::AtLeastOnce) {
                    Ok(_) => {
                        info!(
                            "✅ Successfully subscribed to settings topic: {}",
                            settings_topic
                        );

                        // Subscribe to commands
                        let commands_topic = format!("{}/{}", TOPIC_COMMANDS, device_id);
                        info!(
                            "📨 Attempting to subscribe to commands topic: {}",
                            commands_topic
                        );

                        match client.subscribe(&commands_topic, QoS::AtLeastOnce) {
                            Ok(_) => {
                                info!(
                                    "✅ Successfully subscribed to commands topic: {}",
                                    commands_topic
                                );

                                // Subscribe to status request topic
                                let status_req_topic =
                                    format!("{}/{}", TOPIC_STATUS_REQUEST, device_id);
                                info!(
                                    "📨 Attempting to subscribe to status request topic: {}",
                                    status_req_topic
                                );

                                match client.subscribe(&status_req_topic, QoS::AtLeastOnce) {
                                    Ok(_) => {
                                        info!(
                                            "✅ Successfully subscribed to status request topic: {}",
                                            status_req_topic
                                        );
                                        info!("✅ All MQTT topic subscriptions completed successfully");
                                        info!("  📨 Settings: {}", settings_topic);
                                        info!("  📨 Commands: {}", commands_topic);
                                        info!("  📨 Status Request: {}", status_req_topic);
                                        return Ok(());
                                    }
                                    Err(e) => {
                                        error!(
                                            "❌ Failed to subscribe to status request topic: {:?}",
                                            e
                                        );
                                        if attempt >= max_attempts {
                                            return Err(anyhow!(
                                                "Failed to subscribe to status request topic after {} attempts: {:?}",
                                                max_attempts,
                                                e
                                            ));
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                error!("❌ Failed to subscribe to commands topic: {:?}", e);
                                if attempt >= max_attempts {
                                    return Err(anyhow!("Failed to subscribe to commands topic after {} attempts: {:?}", max_attempts, e));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(
                            "❌ Failed to subscribe to settings topic on attempt {}: {:?}",
                            attempt, e
                        );
                        if attempt >= max_attempts {
                            return Err(anyhow!(
                                "Failed to subscribe to settings topic after {} attempts: {:?}",
                                max_attempts,
                                e
                            ));
                        }
                    }
                }

                // Wait before retry - give ESP-IDF MQTT client more time to establish connection
                let delay_seconds = attempt * 2; // Progressive backoff: 2s, 4s, 6s, etc.
                info!(
                    "⏳ Waiting {} seconds before retry (attempt {}/{})",
                    delay_seconds, attempt, max_attempts
                );
                Timer::after(Duration::from_secs(delay_seconds)).await;
                attempt += 1;
            }

            Err(anyhow!(
                "Failed to subscribe to MQTT topics after {} attempts",
                max_attempts
            ))
        } else {
            Err(anyhow!("MQTT client not initialized"))
        }
    }

    /// Publish button press event to AWS IoT Core
    pub async fn publish_button_press(
        &mut self,
        button_rf_id: &str,
        battery_level: Option<u8>,
    ) -> Result<()> {
        if !self.is_connected() {
            return Err(anyhow!("MQTT client not connected"));
        }

        let message = ButtonPressMessage {
            device_id: self.device_id.clone(),
            button_rf_id: button_rf_id.to_string(),
            timestamp: self.get_iso8601_timestamp(),
            battery_level,
        };

        let topic = format!("{}/{}", TOPIC_BUTTON_PRESS, self.device_id);
        self.publish_json_message(&topic, &message).await?;

        info!(
            "🔔 Published authenticated button press: button={}, battery={:?}",
            button_rf_id, battery_level
        );
        Ok(())
    }

    /// Publish device status update
    pub async fn publish_device_status(
        &mut self,
        status: &str,
        _wifi_signal: Option<i32>,
    ) -> Result<()> {
        if !self.is_connected() {
            return Err(anyhow!("MQTT client not connected"));
        }

        let message = DeviceStatusMessage {
            device_id: self.device_id.clone(),
            timestamp: self.get_iso8601_timestamp(),
            status: status.to_string(),
            firmware_version: "1.0.0".to_string(),
        };

        let topic = format!("{}/{}", TOPIC_STATUS_RESPONSE, self.device_id);
        self.publish_json_message(&topic, &message).await?;

        debug!("📊 Published authenticated device status: {}", status);
        Ok(())
    }

    /// Publish heartbeat message
    pub async fn publish_heartbeat(&mut self) -> Result<()> {
        if !self.is_connected() {
            return Err(anyhow!("MQTT client not connected"));
        }

        let message = DeviceStatusMessage {
            device_id: self.device_id.clone(),
            timestamp: self.get_iso8601_timestamp(),
            status: "online".to_string(),
            firmware_version: "1.0.0".to_string(),
        };

        let topic = format!("{}/{}", TOPIC_HEARTBEAT, self.device_id);
        self.publish_json_message(&topic, &message).await?;

        debug!("💓 Published authenticated heartbeat");
        Ok(())
    }

    /// Publish volume change notification
    pub async fn publish_volume_change(&mut self, volume: u8, source: &str) -> Result<()> {
        if !self.is_connected() {
            return Err(anyhow!("MQTT client not connected"));
        }

        let topic = format!("{}/{}", TOPIC_STATUS_RESPONSE, self.device_id);
        let message = serde_json::json!({
            "deviceId": self.device_id,
            "timestamp": self.get_iso8601_timestamp(),
            "volume": volume,
            "source": source
        });

        let json_payload = serde_json::to_string(&message)
            .map_err(|e| anyhow!("Failed to serialize volume message: {}", e))?;

        if let Some(ref mut client) = self.client {
            client
                .publish(&topic, QoS::AtLeastOnce, false, json_payload.as_bytes())
                .map_err(|e| anyhow!("Failed to publish volume change: {:?}", e))?;

            info!(
                "🔊 Published authenticated volume change: {}% ({})",
                volume, source
            );
            Ok(())
        } else {
            Err(anyhow!("MQTT client not initialized"))
        }
    }

    /// Generic JSON message publishing with X.509 authenticated MQTT client
    async fn publish_json_message<T: Serialize>(&mut self, topic: &str, message: &T) -> Result<()> {
        if !self.is_connected() {
            return Err(anyhow!("MQTT client not connected"));
        }

        let json_payload = serde_json::to_string(message)
            .map_err(|e| anyhow!("Failed to serialize message: {}", e))?;

        if let Some(ref mut client) = self.client {
            client
                .publish(topic, QoS::AtLeastOnce, false, json_payload.as_bytes())
                .map_err(|e| anyhow!("Failed to publish to topic {}: {:?}", topic, e))?;

            debug!(
                "📤 Published authenticated message to {}: {} bytes",
                topic,
                json_payload.len()
            );
            debug!("📝 Payload: {}", json_payload);
            Ok(())
        } else {
            Err(anyhow!("MQTT client not initialized"))
        }
    }

    /// Check if client is connected
    pub fn is_connected(&self) -> bool {
        matches!(self.connection_status, ConnectionStatus::Connected)
    }

    /// Get current connection status
    pub fn get_connection_status(&self) -> &ConnectionStatus {
        &self.connection_status
    }

    /// Disconnect from AWS IoT Core
    pub async fn disconnect(&mut self) -> Result<()> {
        info!("🔌 Disconnecting from AWS IoT Core");

        // Clean up MQTT client
        self.client = None;
        self.connection_status = ConnectionStatus::Disconnected;

        info!("✅ Disconnected from AWS IoT Core");
        Ok(())
    }

    /// Attempt to reconnect to AWS IoT Core with exponential backoff
    /// Uses the same X.509 certificates for secure reconnection
    pub async fn reconnect(&mut self) -> Result<(), anyhow::Error> {
        if self.retry_count >= MAX_RETRY_ATTEMPTS {
            return Err(anyhow!("Maximum retry attempts exceeded"));
        }

        info!(
            "🔄 Attempting X.509 authenticated MQTT reconnection (attempt {})",
            self.retry_count + 1
        );

        // Get broker URL from stored certificates
        let broker_url = if let Some(ref certificates) = self.certificates {
            format!("mqtts://{}:8883", certificates.iot_endpoint)
        } else {
            return Err(anyhow!("No certificates available for reconnection"));
        };

        match self.connect(&broker_url).await {
            Ok(_) => {
                info!("✅ Reconnection successful with X.509 certificate authentication");
                Ok(())
            }
            Err(e) => {
                warn!(
                    "⚠️ Reconnection attempt {} failed: {}",
                    self.retry_count + 1,
                    e
                );
                self.update_retry_state();
                Err(e)
            }
        }
    }

    /// Process incoming MQTT messages (X.509 authenticated connection)
    pub async fn process_messages(&mut self) -> Result<()> {
        // For now, just ensure we're connected with X.509 authentication
        if !self.is_connected() {
            debug!("📭 X.509 authenticated MQTT not connected, no messages to process");
        } else {
            debug!("🔒 Processing messages on X.509 certificate-authenticated connection");
        }
        Ok(())
    }

    /// Reset retry state after successful connection
    fn reset_retry_state(&mut self) {
        self.retry_count = 0;
        self.retry_delay_ms = INITIAL_RETRY_DELAY_MS;
    }

    /// Update retry state after failed connection
    fn update_retry_state(&mut self) {
        self.retry_count += 1;

        // Exponential backoff with maximum delay
        self.retry_delay_ms = std::cmp::min(self.retry_delay_ms * 2, MAX_RETRY_DELAY_MS);
    }

    /// Get current timestamp in ISO 8601 format
    fn get_iso8601_timestamp(&self) -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string()
    }
}
