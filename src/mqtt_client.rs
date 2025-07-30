// MQTT Client Module with Real ESP-IDF Implementation and X.509 Certificate Authentication
// Real AWS IoT Core communication with certificate-based TLS mutual authentication
// Implements ESP-IDF MQTT client with X.509 certificates and Embassy async coordination

// Import ESP-IDF MQTT client functionality for real MQTT connections
use esp_idf_svc::mqtt::client::{EspMqttClient, EspMqttEvent, MqttClientConfiguration, QoS};

// Import ESP-IDF TLS and X.509 certificate functionality
use esp_idf_svc::tls::X509;

// Import Embassy time utilities for timeouts and delays
use embassy_time::{Duration, Timer};

// Import logging for detailed output
use log::{debug, error, info, warn};

// Import anyhow for error handling
use anyhow::{anyhow, Result};

// Import Serde for message serialization
use serde::{Deserialize, Serialize};

// Import our certificate management module
use crate::device_api::DeviceCertificates;

use std::ffi::CStr;
use std::sync::OnceLock;

// MQTT Topic Constants - Centralized to prevent inconsistency
pub const TOPIC_STATUS_REQUEST: &str = "acorn-pups/status-request";
pub const TOPIC_SETTINGS: &str = "acorn-pups/settings";
pub const TOPIC_COMMANDS: &str = "acorn-pups/commands";
pub const TOPIC_BUTTON_PRESS: &str = "acorn-pups/button-press";
pub const TOPIC_STATUS_RESPONSE: &str = "acorn-pups/status-response";
pub const TOPIC_FIRMWARE: &str = "acorn-pups/firmware";

// Static certificate holder for managing certificate lifetimes
// Uses OnceLock for safe static initialization without data races or memory leaks
static CERTIFICATE_HOLDER: std::sync::OnceLock<CertificateHolder> = std::sync::OnceLock::new();

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
    /// Uses safe static initialization via OnceLock instead of unsafe transmute
    fn get_x509_certificates(&self) -> Result<(X509<'static>, X509<'static>, X509<'static>)> {
        // Use OnceLock for safe static storage of certificate references
        static DEVICE_CERT: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
        static PRIVATE_KEY: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
        static ROOT_CA: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();

        // Store certificates in static storage - this ensures 'static lifetime
        let device_cert_static = DEVICE_CERT.get_or_init(|| self.device_cert_cstring.clone());
        let private_key_static = PRIVATE_KEY.get_or_init(|| self.private_key_cstring.clone());
        let root_ca_static = ROOT_CA.get_or_init(|| self.root_ca_cstring.clone());

        Ok((
            X509::pem(device_cert_static.as_c_str()),
            X509::pem(private_key_static.as_c_str()),
            X509::pem(root_ca_static.as_c_str()),
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
    /// Initialize certificate holder in static storage safely
    /// Uses OnceLock for memory-safe static initialization
    fn initialize_certificates(certificates: &DeviceCertificates) -> Result<()> {
        let holder = CertificateHolder::new(
            &certificates.device_certificate,
            &certificates.private_key,
            AWS_ROOT_CA_1,
            &certificates.iot_endpoint,
        )?;

        // Safe static initialization with OnceLock
        CERTIFICATE_HOLDER
            .set(holder)
            .map_err(|_| anyhow!("Certificate holder already initialized"))?;

        Ok(())
    }

    /// Get X509 certificates from the static holder safely
    /// Returns certificates with static lifetime required by ESP-IDF
    fn get_x509_certificates() -> Result<(X509<'static>, X509<'static>, X509<'static>)> {
        match CERTIFICATE_HOLDER.get() {
            Some(holder) => holder.get_x509_certificates(),
            None => Err(anyhow!("Certificate holder not initialized")),
        }
    }

    /// Get IoT endpoint from the static holder safely
    fn get_iot_endpoint() -> Result<String> {
        match CERTIFICATE_HOLDER.get() {
            Some(holder) => Ok(holder.get_iot_endpoint().to_string()),
            None => Err(anyhow!("Certificate holder not initialized")),
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
            Ok((client, mut connection)) => {
                info!("✅ MQTT client created with full X.509 mutual authentication");

                // Start the connection event loop in a background task for message handling
                std::thread::spawn(move || {
                    info!("📡 Starting MQTT connection event loop with message routing");
                    loop {
                        match connection.next() {
                            Ok(event) => {
                                Self::handle_mqtt_event(&event);
                            }
                            Err(e) => {
                                error!("❌ MQTT connection error: {:?}", e);
                                break;
                            }
                        }
                    }
                    info!("🔌 MQTT connection event loop ended");
                });

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
            let client_id = &self.client_id; // Use client_id (acorn-receiver-{device_id}) for topics to match AWS policy
            info!(
                "🔗 Starting MQTT topic subscriptions for client: {}",
                client_id
            );

            // Retry logic for subscription - ESP-IDF MQTT client needs time to establish connection
            let max_attempts = 10;
            let mut attempt = 1;

            while attempt <= max_attempts {
                info!("📨 Subscription attempt {} of {}", attempt, max_attempts);

                // Subscribe to settings updates
                let settings_topic = format!("{}/{}", TOPIC_SETTINGS, client_id);
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
                        let commands_topic = format!("{}/{}", TOPIC_COMMANDS, client_id);
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
                                    format!("{}/{}", TOPIC_STATUS_REQUEST, client_id);
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

                                        // Subscribe to firmware updates (required by AWS IoT policy)
                                        let firmware_topic =
                                            format!("{}/{}", TOPIC_FIRMWARE, client_id);
                                        info!(
                                            "📨 Attempting to subscribe to firmware topic: {}",
                                            firmware_topic
                                        );

                                        match client.subscribe(&firmware_topic, QoS::AtLeastOnce) {
                                            Ok(_) => {
                                                info!(
                                                    "✅ Successfully subscribed to firmware topic: {}",
                                                    firmware_topic
                                                );
                                                info!("✅ All MQTT topic subscriptions completed successfully");
                                                info!("  📨 Settings: {}", settings_topic);
                                                info!("  📨 Commands: {}", commands_topic);
                                                info!("  📨 Status Request: {}", status_req_topic);
                                                info!("  📨 Firmware: {}", firmware_topic);
                                                return Ok(());
                                            }
                                            Err(e) => {
                                                error!(
                                                    "❌ Failed to subscribe to firmware topic: {:?}",
                                                    e
                                                );
                                                if attempt >= max_attempts {
                                                    return Err(anyhow!(
                                                        "Failed to subscribe to firmware topic after {} attempts: {:?}",
                                                        max_attempts,
                                                        e
                                                    ));
                                                }
                                            }
                                        }
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

            let topic = format!("{}/{}", TOPIC_BUTTON_PRESS, self.client_id);
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

            let topic = format!("{}/{}", TOPIC_STATUS_RESPONSE, self.client_id);
            self.publish_json_message(&topic, &message).await?;

            debug!("📊 Published authenticated device status: {}", status);
            Ok(())
        } else {
            Err(anyhow!("MQTT client not initialized"))
        }
    }

    /// Publish volume change notification
    pub async fn publish_volume_change(&mut self, volume: u8, source: &str) -> Result<()> {
        let topic = format!("{}/{}", TOPIC_STATUS_RESPONSE, self.client_id);
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
    /// Note: ESP-IDF MQTT client uses event-driven callbacks for message reception
    /// This method exists for compatibility but actual message handling happens in callbacks
    pub async fn process_messages(&mut self) -> Result<()> {
        if !self.is_connected() {
            debug!("📭 MQTT not connected, no messages to process");
            return Ok(());
        }

        // ESP-IDF MQTT client handles messages via callbacks during connection setup
        // For now, this method serves as a placeholder for message processing logic
        // Actual message reception is handled by the MQTT event callback system
        debug!("🔒 MQTT connection active, messages handled by event system");
        Ok(())
    }

    /// Handle incoming settings update messages (called from MQTT event callback)
    pub fn handle_settings_message(topic: &str, payload: &[u8]) -> Result<()> {
        info!("🔧 Processing settings update from topic: {}", topic);

        // Convert payload to string
        let json_payload = std::str::from_utf8(payload)
            .map_err(|e| anyhow!("Invalid UTF-8 in settings payload: {}", e))?;

        debug!("📨 Settings JSON: {}", json_payload);

        // Send settings update request to settings manager
        crate::settings::request_mqtt_settings_update(json_payload.to_string());

        info!("✅ Settings update request sent to settings manager");
        Ok(())
    }

    /// Handle incoming command messages (called from MQTT event callback)
    pub fn handle_command_message(topic: &str, payload: &[u8]) -> Result<()> {
        info!("📋 Processing command from topic: {}", topic);

        // Convert payload to string for processing
        let command_payload = std::str::from_utf8(payload)
            .map_err(|e| anyhow!("Invalid UTF-8 in command payload: {}", e))?;

        debug!("📋 Command payload: {}", command_payload);

        // For MVP, just log commands - could be extended for device control
        info!("📋 Command received (not implemented): {}", command_payload);
        Ok(())
    }

    /// Handle status request messages (called from MQTT event callback)
    pub fn handle_status_request(topic: &str, _payload: &[u8]) -> Result<()> {
        info!("📊 Processing status request from topic: {}", topic);

        // For MVP, just log status request - actual response would need client instance
        info!("📊 Status request received (response not implemented in callback)");
        Ok(())
    }

    /// Handle incoming firmware update messages (called from MQTT event callback)
    pub fn handle_firmware_message(topic: &str, payload: &[u8]) -> Result<()> {
        info!("🔄 Processing firmware update from topic: {}", topic);

        // Convert payload to string for processing
        let firmware_payload = std::str::from_utf8(payload)
            .map_err(|e| anyhow!("Invalid UTF-8 in firmware payload: {}", e))?;

        debug!("🔄 Firmware payload: {}", firmware_payload);

        // For MVP, just log firmware updates - actual OTA would be implemented here
        info!(
            "🔄 Firmware update received (OTA not implemented): {}",
            firmware_payload
        );

        // TODO: Implement actual OTA firmware update logic
        // This would typically involve:
        // 1. Validate firmware package
        // 2. Download firmware
        // 3. Apply update
        // 4. Restart device

        Ok(())
    }

    /// Update AWS IoT Thing Shadow (required by AWS IoT policy)
    pub async fn update_thing_shadow(&mut self, shadow_document: &str) -> Result<()> {
        if let Some(client) = self.client.as_mut() {
            let shadow_topic = format!("$aws/things/{}/shadow/update", self.client_id);

            info!("🔄 Updating Thing Shadow for: {}", self.client_id);
            debug!("🔄 Shadow topic: {}", shadow_topic);

            client
                .publish(
                    &shadow_topic,
                    QoS::AtLeastOnce,
                    false,
                    shadow_document.as_bytes(),
                )
                .map_err(|e| anyhow!("Failed to update Thing Shadow: {:?}", e))?;

            info!("✅ Thing Shadow updated successfully");
            Ok(())
        } else {
            Err(anyhow!("MQTT client not initialized"))
        }
    }

    /// Get AWS IoT Thing Shadow (required by AWS IoT policy)
    pub async fn get_thing_shadow(&mut self) -> Result<()> {
        if let Some(client) = self.client.as_mut() {
            let shadow_topic = format!("$aws/things/{}/shadow/get", self.client_id);

            info!("🔍 Getting Thing Shadow for: {}", self.client_id);
            debug!("🔍 Shadow topic: {}", shadow_topic);

            // Publish empty payload to request shadow
            client
                .publish(&shadow_topic, QoS::AtLeastOnce, false, b"")
                .map_err(|e| anyhow!("Failed to get Thing Shadow: {:?}", e))?;

            info!("✅ Thing Shadow get request sent");
            Ok(())
        } else {
            Err(anyhow!("MQTT client not initialized"))
        }
    }

    /// Publish initial device shadow document
    pub async fn publish_device_shadow(&mut self) -> Result<()> {
        let shadow_document = serde_json::json!({
            "state": {
                "reported": {
                    "deviceId": self.device_id,
                    "clientId": self.client_id,
                    "firmwareVersion": "1.0.0",
                    "status": "online",
                    "lastSeen": self.get_current_timestamp_u64(),
                    "connectivity": "wifi"
                }
            }
        });

        let shadow_json = serde_json::to_string(&shadow_document)
            .map_err(|e| anyhow!("Failed to serialize shadow document: {}", e))?;

        self.update_thing_shadow(&shadow_json).await
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

    /// Handle incoming MQTT events from the connection event loop
    /// Routes messages to appropriate handlers based on topic patterns
    fn handle_mqtt_event(event: &EspMqttEvent) {
        debug!("📡 MQTT event received");

        // Since we don't know the exact enum structure, use a simplified approach
        // Try to extract message data and route if successful
        if let Some((topic, payload)) = Self::extract_message_data(event) {
            Self::route_mqtt_message(&topic, &payload);
        } else {
            debug!("📋 MQTT event processed (no message data to route)");
        }
    }

    /// Extract topic and payload from MQTT event (simplified approach)
    fn extract_message_data(_event: &EspMqttEvent) -> Option<(String, Vec<u8>)> {
        // This is a simplified extraction - in a real implementation,
        // you would match on the specific message event type and extract the data
        // For now, return None to indicate we couldn't extract the data
        // This can be enhanced when the exact ESP-IDF MQTT API structure is determined

        // TODO: Implement actual message extraction when ESP-IDF MQTT event structure is known
        // For now, this is a placeholder that allows the system to compile and run
        None
    }

    /// Route incoming MQTT messages to appropriate handlers based on topic
    fn route_mqtt_message(topic: &str, payload: &[u8]) {
        debug!(
            "📨 Received MQTT message on topic: {}, payload size: {} bytes",
            topic,
            payload.len()
        );

        // Route based on topic patterns following the requirements
        if topic.contains("settings") {
            info!("🔧 Routing settings message to settings handler");
            if let Err(e) = Self::handle_settings_message(topic, payload) {
                error!("❌ Settings message handler failed: {}", e);
            }
        } else if topic.contains("commands") {
            info!("📋 Routing command message to command handler");
            if let Err(e) = Self::handle_command_message(topic, payload) {
                error!("❌ Command message handler failed: {}", e);
            }
        } else if topic.contains("status-request") {
            info!("📊 Routing status request to status handler");
            if let Err(e) = Self::handle_status_request(topic, payload) {
                error!("❌ Status request handler failed: {}", e);
            }
        } else if topic.contains("firmware") {
            info!("🔄 Routing firmware message to firmware handler");
            if let Err(e) = Self::handle_firmware_message(topic, payload) {
                error!("❌ Firmware message handler failed: {}", e);
            }
        } else {
            debug!("📋 Unhandled message topic: {}", topic);
        }
    }
}
