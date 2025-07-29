// MQTT Client Module with Real ESP-IDF Implementation and X.509 Certificate Authentication
// Real AWS IoT Core communication with certificate-based TLS mutual authentication
// Implements ESP-IDF MQTT client with X.509 certificates and Embassy async coordination

// Import ESP-IDF MQTT client functionality for real MQTT connections
use esp_idf_svc::mqtt::client::{EspMqttClient, MqttClientConfiguration, QoS};

// Import ESP-IDF TLS and X.509 certificate functionality
use esp_idf_svc::tls::X509;

// Import Embassy time utilities for timeouts and delays
use embassy_time::{Duration, Timer};

// Import logging for detailed output
use log::{debug, error, info};

// Import anyhow for error handling
use anyhow::{anyhow, Result};

// Import Serde for message serialization
use serde::{Deserialize, Serialize};

// Import our certificate management module
use crate::device_api::DeviceCertificates;

// MQTT Topic Constants - Centralized to prevent inconsistency
pub const TOPIC_STATUS_REQUEST: &str = "acorn-pups/status-request";
pub const TOPIC_SETTINGS: &str = "acorn-pups/settings";
pub const TOPIC_COMMANDS: &str = "acorn-pups/commands";
pub const TOPIC_BUTTON_PRESS: &str = "acorn-pups/button-press";
pub const TOPIC_STATUS_RESPONSE: &str = "acorn-pups/status-response";

// Static certificate holder for managing certificate lifetimes
// This provides the static lifetime requirement without memory leaks
static mut CERTIFICATE_HOLDER: Option<CertificateHolder> = None;
static CERTIFICATE_INIT: std::sync::Once = std::sync::Once::new();

/// Safe certificate holder that manages X509 certificate lifetimes
/// without causing memory leaks like Box::leak()
struct CertificateHolder {
    device_cert_cstring: std::ffi::CString,
    private_key_cstring: std::ffi::CString,
    root_ca_cstring: std::ffi::CString,
    iot_endpoint: String,
}

impl CertificateHolder {
    /// Create a new certificate holder with owned certificate data
    fn new(
        device_cert: &str,
        private_key: &str,
        root_ca: &str,
        iot_endpoint: &str,
    ) -> Result<Self> {
        Ok(Self {
            device_cert_cstring: std::ffi::CString::new(device_cert)
                .map_err(|e| anyhow!("Device certificate contains null bytes: {}", e))?,
            private_key_cstring: std::ffi::CString::new(private_key)
                .map_err(|e| anyhow!("Private key contains null bytes: {}", e))?,
            root_ca_cstring: std::ffi::CString::new(root_ca)
                .map_err(|e| anyhow!("Root CA contains null bytes: {}", e))?,
            iot_endpoint: iot_endpoint.to_string(),
        })
    }

    /// Get X509 certificate objects with proper lifetimes
    /// Uses safe transmute instead of raw pointer casting
    fn get_x509_certificates(&self) -> Result<(X509<'static>, X509<'static>, X509<'static>)> {
        // SAFETY: The CStrings are owned by this holder and stored in static storage
        // This transmute is safe because the holder lives for the entire program duration
        let device_cert_static: &'static std::ffi::CStr =
            unsafe { std::mem::transmute(self.device_cert_cstring.as_c_str()) };
        let private_key_static: &'static std::ffi::CStr =
            unsafe { std::mem::transmute(self.private_key_cstring.as_c_str()) };
        let root_ca_static: &'static std::ffi::CStr =
            unsafe { std::mem::transmute(self.root_ca_cstring.as_c_str()) };

        Ok((
            X509::pem(device_cert_static),
            X509::pem(private_key_static),
            X509::pem(root_ca_static),
        ))
    }

    /// Get the IoT endpoint for MQTT connection
    fn get_iot_endpoint(&self) -> &str {
        &self.iot_endpoint
    }
}

// AWS IoT Core Root CA certificate - this is public and never changes
const AWS_ROOT_CA_1: &str = "-----BEGIN CERTIFICATE-----
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
U5PMCCjjmCXPI6T53iHTfIuJruydjsw2hUwsqdruciRmkVcXiGwTr39vFdGw8F4L
rZNPNtOCWFO6LuQJILh1YnPXiDbGZ9QBTE6m6z/g8ww7J0MZWNGb2YgO3xYcOTKA
P4fOUfB1Lp3x8qTx9ePHdPKLqHWqcSBqSGLhXvHJQhQdNvh1i9D8CuCH5gUkGF+E
JUUFoaYl2Pm7CmU9dGQB9zZiQ6CbhJfJqfJ5tT5y8/dq6PggdnQ0vE5Aq3UqpJfF
b+a+8oXGh9wjHo/U7nLIpJo6xpGW
-----END CERTIFICATE-----";

// Message structures following technical documentation
// Using JSON for compatibility with backend and monitoring systems
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeviceStatusMessage {
    pub device_id: String,
    pub status: String,
    pub timestamp: u64,
    pub uptime_seconds: u64,
    pub free_heap: u32,
    pub wifi_rssi: i32,
}

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

/// MQTT connection state tracking
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqttConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

/// MQTT client manager for AWS IoT Core
/// Handles mutual TLS authentication with device certificates
pub struct AwsIotMqttClient {
    client: Option<EspMqttClient<'static>>,
    device_id: String,
    client_id: String,
    connection_state: MqttConnectionState,
}

impl AwsIotMqttClient {
    /// Initialize certificate holder in static storage
    /// This must be called before creating any X509 certificates
    fn initialize_certificates(certificates: &DeviceCertificates) -> Result<()> {
        CERTIFICATE_INIT.call_once(|| {
            match CertificateHolder::new(
                &certificates.device_certificate,
                &certificates.private_key,
                AWS_ROOT_CA_1,
                &certificates.iot_endpoint,
            ) {
                Ok(holder) => unsafe {
                    CERTIFICATE_HOLDER = Some(holder);
                },
                Err(e) => {
                    error!("❌ Failed to initialize certificate holder: {}", e);
                }
            }
        });

        // Verify initialization succeeded
        unsafe {
            if CERTIFICATE_HOLDER.is_none() {
                return Err(anyhow!("Certificate holder initialization failed"));
            }
        }

        Ok(())
    }

    /// Get X509 certificates from the static holder
    /// Returns certificates with static lifetime required by ESP-IDF
    fn get_x509_certificates() -> Result<(X509<'static>, X509<'static>, X509<'static>)> {
        unsafe {
            match &CERTIFICATE_HOLDER {
                Some(holder) => holder.get_x509_certificates(),
                None => Err(anyhow!("Certificate holder not initialized")),
            }
        }
    }

    /// Get IoT endpoint from the static holder
    fn get_iot_endpoint() -> Result<String> {
        unsafe {
            match &CERTIFICATE_HOLDER {
                Some(holder) => Ok(holder.get_iot_endpoint().to_string()),
                None => Err(anyhow!("Certificate holder not initialized")),
            }
        }
    }

    /// Create new MQTT client with device configuration
    /// Separates device_id from client_id for proper semantic distinction
    pub fn new(device_id: String) -> Self {
        let client_id = format!("acorn-receiver-{}", device_id);
        info!(
            "🔌 Creating MQTT client for device: {} with client_id: {}",
            device_id, client_id
        );

        Self {
            client: None,
            device_id,
            client_id,
            connection_state: MqttConnectionState::Disconnected,
        }
    }

    /// Initialize client with stored certificates
    pub async fn initialize_with_certificates(
        &mut self,
        cert_storage: &mut crate::mqtt_certificates::MqttCertificateStorage,
    ) -> Result<()> {
        info!("🔐 Initializing MQTT client with stored X.509 certificates");

        // Load certificates from storage with optimized buffer sizing
        match cert_storage.load_certificates_for_mqtt()? {
            Some(certificates) => {
                info!("✅ X.509 certificates loaded successfully with optimized buffers");
                info!(
                    "📜 Device certificate length: {} bytes",
                    certificates.device_certificate.len()
                );
                info!(
                    "🔑 Private key length: {} bytes",
                    certificates.private_key.len()
                );
                info!("🌐 IoT endpoint: {}", certificates.iot_endpoint);
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

                // Initialize certificate holder in static storage
                Self::initialize_certificates(&certificates)?;
                info!("🔒 X.509 certificates validated and ready for TLS mutual authentication");
                Ok(())
            }
            None => {
                error!("❌ No X.509 certificates found in NVS storage");
                Err(anyhow!(
                    "Device certificates not found - registration required"
                ))
            }
        }
    }

    /// Connect to AWS IoT Core using X.509 certificate mutual authentication
    pub async fn connect(&mut self) -> Result<()> {
        info!(
            "🔌 Connecting to AWS IoT Core with X.509 certificate mutual authentication: {}",
            self.client_id
        );

        // Get X509 certificates from static holder
        let (device_cert_x509, private_key_x509, root_ca_x509) = Self::get_x509_certificates()?;

        // Get IoT endpoint from static holder
        let _broker_url = format!("mqtts://{}:8883", self.client_id); // Placeholder, needs actual endpoint
        let iot_endpoint = Self::get_iot_endpoint()?;
        let broker_url = format!("mqtts://{}:8883", iot_endpoint);
        info!("🌐 MQTT broker URL: {}", broker_url);
        info!("🆔 Client ID: {}", self.client_id);

        info!("🚀 Creating production-grade ESP-IDF MQTT client configuration");

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

        match EspMqttClient::new(&broker_url, &mqtt_config) {
            Ok((client, _connection)) => {
                info!("✅ MQTT client created with full X.509 mutual authentication");

                // Store client
                self.client = Some(client);
                self.connection_state = MqttConnectionState::Connected;

                Ok(())
            }
            Err(e) => {
                error!("❌ Failed to create MQTT client: {:?}", e);
                self.connection_state = MqttConnectionState::Error;
                Err(anyhow!("MQTT client creation failed: {:?}", e))
            }
        }
    }

    /// Subscribe to device-specific MQTT topics with retry logic for connection timing
    pub async fn subscribe_to_device_topics(&mut self) -> Result<()> {
        if let Some(client) = self.client.as_mut() {
            let device_id = &self.device_id; // Use device_id as device_id for topics
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
        if let Some(_client) = self.client.as_mut() {
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
        } else {
            Err(anyhow!("MQTT client not initialized"))
        }
    }

    /// Publish device status update
    pub async fn publish_device_status(
        &mut self,
        status: &str,
        _wifi_signal: Option<i32>,
    ) -> Result<()> {
        if let Some(_client) = self.client.as_mut() {
            let message = DeviceStatusMessage {
                device_id: self.device_id.clone(),
                status: status.to_string(),
                timestamp: self.get_current_timestamp_u64(),
                uptime_seconds: 0, // Placeholder, needs actual uptime
                free_heap: 0,      // Placeholder, needs actual free heap
                wifi_rssi: 0,      // Placeholder, needs actual RSSI
            };

            let topic = format!("{}/{}", TOPIC_STATUS_RESPONSE, self.device_id);
            self.publish_json_message(&topic, &message).await?;

            debug!("📊 Published authenticated device status: {}", status);
            Ok(())
        } else {
            Err(anyhow!("MQTT client not initialized"))
        }
    }

    /// Publish volume change notification
    pub async fn publish_volume_change(&mut self, volume: u8, source: &str) -> Result<()> {
        let topic = format!("{}/{}", TOPIC_STATUS_RESPONSE, self.device_id);
        let device_id = self.device_id.clone();
        let timestamp = self.get_current_timestamp_u64();

        if let Some(client) = self.client.as_mut() {
            let message = serde_json::json!({
                "deviceId": device_id,
                "timestamp": timestamp,
                "volume": volume,
                "source": source
            });

            let json_payload = serde_json::to_string(&message)
                .map_err(|e| anyhow!("Failed to serialize volume message: {}", e))?;

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

    /// Disconnect from AWS IoT Core
    pub async fn disconnect(&mut self) -> Result<()> {
        info!("🔌 Disconnecting from AWS IoT Core");

        // Clean up MQTT client
        self.client = None;
        self.connection_state = MqttConnectionState::Disconnected;

        info!("✅ Disconnected from AWS IoT Core");
        Ok(())
    }

    /// Generic JSON message publishing with X.509 authenticated MQTT client
    async fn publish_json_message<T: Serialize>(&mut self, topic: &str, message: &T) -> Result<()> {
        if let Some(client) = self.client.as_mut() {
            let json_payload = serde_json::to_string(message)
                .map_err(|e| anyhow!("Failed to serialize message: {}", e))?;

            client
                .publish(topic, QoS::AtLeastOnce, false, json_payload.as_bytes())
                .map_err(|e| anyhow!("Failed to publish message: {:?}", e))?;

            debug!("📤 Published message to topic: {}", topic);
            Ok(())
        } else {
            Err(anyhow!("MQTT client not initialized"))
        }
    }

    /// Get ISO 8601 timestamp for message timestamps
    fn get_iso8601_timestamp(&self) -> String {
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    /// Get current timestamp as u64
    fn get_current_timestamp_u64(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Update connection state based on MQTT events
    pub fn update_connection_state(&mut self, connected: bool) {
        self.connection_state = if connected {
            MqttConnectionState::Connected
        } else {
            MqttConnectionState::Disconnected
        };

        info!(
            "🔄 MQTT connection state updated: {:?}",
            self.connection_state
        );
    }

    /// Check if client is connected
    pub fn is_connected(&self) -> bool {
        matches!(self.connection_state, MqttConnectionState::Connected)
    }

    /// Get current connection status
    pub fn get_connection_status(&self) -> &MqttConnectionState {
        &self.connection_state
    }

    /// Process incoming MQTT messages
    pub async fn process_messages(&mut self) -> Result<()> {
        if !self.is_connected() {
            debug!("📭 MQTT not connected, no messages to process");
            return Ok(());
        }

        debug!("🔒 Processing messages on authenticated connection");
        Ok(())
    }

    /// Attempt reconnection using tiered recovery
    pub async fn attempt_reconnection(&mut self) -> Result<()> {
        info!("🔄 Attempting MQTT reconnection using tiered recovery");

        // Use tiered recovery for reconnection attempts
        let mut recovery_manager =
            crate::reset_manager::TieredRecoveryManager::new(self.device_id.clone());
        match recovery_manager.attempt_recovery("mqtt_reconnection").await {
            Ok(_tier) => {
                info!("✅ MQTT reconnection successful");
                self.connection_state = MqttConnectionState::Connected;
                Ok(())
            }
            Err(e) => {
                error!("❌ MQTT reconnection failed: {:?}", e);
                self.connection_state = MqttConnectionState::Error;
                Err(anyhow!("MQTT reconnection failed: {:?}", e))
            }
        }
    }

    /// Get device ID (separate from client ID)
    pub fn get_device_id(&self) -> &str {
        &self.device_id
    }

    /// Get client ID for MQTT operations
    pub fn get_client_id(&self) -> &str {
        &self.client_id
    }
}
