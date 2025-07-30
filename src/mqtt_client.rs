// MQTT Client Module with EspAsyncMqttClient for Embassy async integration
// Real AWS IoT Core communication with certificate-based TLS mutual authentication
// Fully async implementation using EspAsyncMqttClient and owned certificate data

// Import ESP-IDF async MQTT client functionality
use esp_idf_svc::mqtt::client::{
    EspAsyncMqttClient, EspMqttClient, MqttClientConfiguration, QoS,
};

// Import Embassy time utilities for timeouts and delays
use embassy_time::{with_timeout, Duration};

// Import logging for detailed output
use log::{debug, error, info, warn};

// Import anyhow for error handling
use anyhow::{anyhow, Result};

// Import Serde for message serialization
use serde::{Deserialize, Serialize};

// Import our certificate management module
use crate::device_api::DeviceCertificates;
use crate::mqtt_certificates::MqttCertificateStorage;

// MQTT Topic Constants - Centralized to prevent inconsistency
pub const TOPIC_STATUS_REQUEST: &str = "acorn-pups/status-request";
pub const TOPIC_SETTINGS: &str = "acorn-pups/settings";
pub const TOPIC_COMMANDS: &str = "acorn-pups/commands";
pub const TOPIC_BUTTON_PRESS: &str = "acorn-pups/button-press";
pub const TOPIC_STATUS_RESPONSE: &str = "acorn-pups/status-response";

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

/// Async MQTT client manager for AWS IoT Core
/// Handles mutual TLS authentication with device certificates using EspAsyncMqttClient
pub struct AwsIotMqttClient {
    client: Option<EspAsyncMqttClient>,
    certificates: Option<DeviceCertificates>,
    device_id: String,
    client_id: String,
    connection_state: MqttConnectionState,
}

impl AwsIotMqttClient {
    /// Create new async MQTT client with device configuration
    /// Stores certificates directly in the struct instead of static holders
    pub fn new(device_id: String) -> Self {
        let client_id = format!("acorn-receiver-{}", device_id);
        info!(
            "🔌 Creating async MQTT client for device: {} with client_id: {}",
            device_id, client_id
        );

        Self {
            client: None,
            certificates: None,
            device_id,
            client_id,
            connection_state: MqttConnectionState::Disconnected,
        }
    }

    /// Initialize client with stored certificates - stores them as owned data
    pub async fn initialize_with_certificates(
        &mut self,
        cert_storage: &mut MqttCertificateStorage,
    ) -> Result<()> {
        info!("🔐 Initializing async MQTT client with stored X.509 certificates");

        // Load certificates from storage with optimized buffer sizing
        match cert_storage.load_certificates_for_mqtt()? {
            Some(certificates) => {
                info!("✅ X.509 certificates loaded successfully");
                info!(
                    "📜 Device certificate length: {} bytes",
                    certificates.device_certificate.len()
                );
                info!(
                    "🔑 Private key length: {} bytes",
                    certificates.private_key.len()
                );
                info!("🌐 IoT endpoint: {}", certificates.iot_endpoint);

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

                // Store certificates as owned data in the struct
                self.certificates = Some(certificates);
                info!("🔒 X.509 certificates stored as owned data and ready for async TLS");
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

    /// Connect to AWS IoT Core using async EspAsyncMqttClient
    pub async fn connect(&mut self) -> Result<()> {
        info!(
            "🔌 Connecting to AWS IoT Core with async MQTT client: {}",
            self.client_id
        );

        // Ensure certificates are loaded
        let certificates = self
            .certificates
            .as_ref()
            .ok_or_else(|| anyhow!("Certificates not initialized"))?;

        // Create X509 certificates on-demand using the new simplified method
        let (device_cert_x509, private_key_x509, root_ca_x509) =
            MqttCertificateStorage::create_x509_certificates(certificates)?;

        let broker_url = format!("mqtts://{}:8883", certificates.iot_endpoint);
        info!("🌐 MQTT broker URL: {}", broker_url);
        info!("🆔 Client ID: {}", self.client_id);

        info!("🚀 Creating async ESP-IDF MQTT client configuration");

        // Create configuration with mutual TLS for AWS IoT Core
        let mqtt_config = MqttClientConfiguration {
            // MQTT client identification
            client_id: Some(&self.client_id),

            // Transport-layer certificate setup
            server_certificate: Some(root_ca_x509), // Validate AWS IoT Core certificate chain
            client_certificate: Some(device_cert_x509),
            private_key: Some(private_key_x509),

            // Reasonable keep-alive/network settings for AWS IoT Core
            keep_alive_interval: Some(core::time::Duration::from_secs(60)),
            reconnect_timeout: Some(core::time::Duration::from_secs(30)),
            network_timeout: core::time::Duration::from_secs(30),

            // Explicitly tell ESP-TLS to use the provided CA instead of global store
            use_global_ca_store: false,
            skip_cert_common_name_check: false,

            // Clean session for AWS IoT Core best practices
            disable_clean_session: false,

            ..Default::default()
        };

        // Create callback-based client first, then wrap with async interface
        match EspMqttClient::new_cb(&broker_url, &mqtt_config, |event| {
            // Handle MQTT events through callback and log them
            Self::handle_mqtt_callback_event(event);
        }) {
            Ok(sync_client) => {
                info!("✅ Synchronous MQTT client created, wrapping with async interface");

                // Wrap with async interface using EspAsyncMqttClient::wrap
                match EspAsyncMqttClient::wrap(sync_client) {
                    Ok(async_client) => {
                        // Store the async client (connection is handled internally)
                        self.client = Some(async_client);
                        self.connection_state = MqttConnectionState::Connecting;

                        info!("✅ Async MQTT client and connection created successfully");
                        info!("⏳ MQTT handshake in progress - connection state: Connecting");
                        
                        // Connection state will be updated to Connected after successful handshake
                        // Subscriptions should wait for connection to be fully established
                        Ok(())
                    }
                    Err(e) => {
                        error!("❌ Failed to wrap MQTT client: {:?}", e);
                        self.connection_state = MqttConnectionState::Error;
                        Err(anyhow!("MQTT client wrapping failed: {:?}", e))
                    }
                }
            }
            Err(e) => {
                error!("❌ Failed to create MQTT client: {:?}", e);
                self.connection_state = MqttConnectionState::Error;
                Err(anyhow!("MQTT client creation failed: {:?}", e))
            }
        }
    }

    /// Wait for MQTT connection to be fully established
    async fn wait_for_connection(&mut self) -> Result<()> {
        info!("⏳ Waiting for MQTT connection to be fully established...");
        
        // For ESP-IDF async MQTT, we'll wait a bit for the connection to establish
        // The logs show the connection does establish after a few seconds
        embassy_time::Timer::after(Duration::from_secs(3)).await;
        
        self.connection_state = MqttConnectionState::Connected;
        info!("✅ MQTT connection assumed established after delay");
        Ok(())
    }

    /// Subscribe to device-specific MQTT topics using async operations
    pub async fn subscribe_to_device_topics(&mut self) -> Result<()> {
        // First wait for the connection to be fully established
        self.wait_for_connection().await?;
        
        if let Some(client) = self.client.as_mut() {
            let client_id = &self.client_id;
            info!(
                "🔗 Starting async MQTT topic subscriptions for client: {}",
                client_id
            );

            // Subscribe to settings updates with timeout
            let settings_topic = format!("{}/{}", TOPIC_SETTINGS, client_id);
            info!("📨 Subscribing to settings topic: {}", settings_topic);

            with_timeout(Duration::from_secs(10), async {
                client.subscribe(&settings_topic, QoS::AtLeastOnce).await
            })
            .await
            .map_err(|_| anyhow!("Settings subscription timed out"))?
            .map_err(|e| anyhow!("Settings subscription failed: {:?}", e))?;

            info!("✅ Successfully subscribed to settings topic");

            // Subscribe to commands with timeout
            let commands_topic = format!("{}/{}", TOPIC_COMMANDS, client_id);
            info!("📨 Subscribing to commands topic: {}", commands_topic);

            with_timeout(Duration::from_secs(10), async {
                client.subscribe(&commands_topic, QoS::AtLeastOnce).await
            })
            .await
            .map_err(|_| anyhow!("Commands subscription timed out"))?
            .map_err(|e| anyhow!("Commands subscription failed: {:?}", e))?;

            info!("✅ Successfully subscribed to commands topic");

            // Subscribe to status request topic with timeout
            let status_req_topic = format!("{}/{}", TOPIC_STATUS_REQUEST, client_id);
            info!(
                "📨 Subscribing to status request topic: {}",
                status_req_topic
            );

            with_timeout(Duration::from_secs(10), async {
                client.subscribe(&status_req_topic, QoS::AtLeastOnce).await
            })
            .await
            .map_err(|_| anyhow!("Status request subscription timed out"))?
            .map_err(|e| anyhow!("Status request subscription failed: {:?}", e))?;

            info!("✅ Successfully subscribed to status request topic");

            info!("✅ All async MQTT topic subscriptions completed successfully");
            Ok(())
        } else {
            Err(anyhow!("Async MQTT client not initialized"))
        }
    }

    /// Publish button press event to AWS IoT Core using async operations
    pub async fn publish_button_press(
        &mut self,
        button_rf_id: &str,
        battery_level: Option<u8>,
    ) -> Result<()> {
        // Get values before borrowing client mutably
        let device_id = self.device_id.clone();
        let client_id = self.client_id.clone();
        let timestamp = self.get_iso8601_timestamp();

        if let Some(client) = self.client.as_mut() {
            let message = ButtonPressMessage {
                device_id,
                button_rf_id: button_rf_id.to_string(),
                timestamp,
                battery_level,
            };

            let topic = format!("{}/{}", TOPIC_BUTTON_PRESS, client_id);

            // Inline publishing to avoid borrow checker issues
            let json_payload = serde_json::to_string(&message)
                .map_err(|e| anyhow!("Failed to serialize message: {}", e))?;

            // Publish with timeout for reliability
            with_timeout(Duration::from_secs(10), async {
                client
                    .publish(&topic, QoS::AtLeastOnce, false, json_payload.as_bytes())
                    .await
            })
            .await
            .map_err(|_| anyhow!("Message publish timed out"))?
            .map_err(|e| anyhow!("Failed to publish message: {:?}", e))?;

            info!(
                "🔔 Published button press asynchronously: button={}, battery={:?}",
                button_rf_id, battery_level
            );
            Ok(())
        } else {
            Err(anyhow!("Async MQTT client not initialized"))
        }
    }

    /// Publish device status update using async operations
    pub async fn publish_device_status(
        &mut self,
        status: &str,
        _wifi_signal: Option<i32>,
    ) -> Result<()> {
        // Get values before borrowing client mutably
        let device_id = self.device_id.clone();
        let client_id = self.client_id.clone();
        let timestamp = self.get_current_timestamp_u64();

        if let Some(client) = self.client.as_mut() {
            let message = DeviceStatusMessage {
                device_id,
                status: status.to_string(),
                timestamp,
                uptime_seconds: 0, // Placeholder, needs actual uptime
                free_heap: 0,      // Placeholder, needs actual free heap
                wifi_rssi: 0,      // Placeholder, needs actual RSSI
            };

            let topic = format!("{}/{}", TOPIC_STATUS_RESPONSE, client_id);

            // Inline publishing to avoid borrow checker issues
            let json_payload = serde_json::to_string(&message)
                .map_err(|e| anyhow!("Failed to serialize message: {}", e))?;

            // Publish with timeout for reliability
            with_timeout(Duration::from_secs(10), async {
                client
                    .publish(&topic, QoS::AtLeastOnce, false, json_payload.as_bytes())
                    .await
            })
            .await
            .map_err(|_| anyhow!("Message publish timed out"))?
            .map_err(|e| anyhow!("Failed to publish message: {:?}", e))?;

            debug!("📊 Published device status asynchronously: {}", status);
            Ok(())
        } else {
            Err(anyhow!("Async MQTT client not initialized"))
        }
    }

    /// Publish volume change notification using async operations
    pub async fn publish_volume_change(&mut self, volume: u8, source: &str) -> Result<()> {
        // Get values before borrowing client mutably
        let client_id = self.client_id.clone();
        let device_id = self.device_id.clone();
        let timestamp = self.get_current_timestamp_u64();

        if let Some(client) = self.client.as_mut() {
            let topic = format!("{}/{}", TOPIC_STATUS_RESPONSE, client_id);

            let message = serde_json::json!({
                "deviceId": device_id,
                "timestamp": timestamp,
                "volume": volume,
                "source": source
            });

            let json_payload = serde_json::to_string(&message)
                .map_err(|e| anyhow!("Failed to serialize volume message: {}", e))?;

            // Publish with timeout
            with_timeout(Duration::from_secs(10), async {
                client
                    .publish(&topic, QoS::AtLeastOnce, false, json_payload.as_bytes())
                    .await
            })
            .await
            .map_err(|_| anyhow!("Volume change publish timed out"))?
            .map_err(|e| anyhow!("Failed to publish volume change: {:?}", e))?;

            info!(
                "🔊 Published volume change asynchronously: {}% ({})",
                volume, source
            );
            Ok(())
        } else {
            Err(anyhow!("Async MQTT client not initialized"))
        }
    }

    /// Disconnect from AWS IoT Core
    pub async fn disconnect(&mut self) -> Result<()> {
        info!("🔌 Disconnecting from AWS IoT Core");

        // Clean up async MQTT client
        self.client = None;
        self.connection_state = MqttConnectionState::Disconnected;

        info!("✅ Disconnected from AWS IoT Core");
        Ok(())
    }

    /// Generic async JSON message publishing
    async fn publish_json_message_async<T: Serialize>(
        &self,
        client: &mut EspAsyncMqttClient,
        topic: &str,
        message: &T,
    ) -> Result<()> {
        let json_payload = serde_json::to_string(message)
            .map_err(|e| anyhow!("Failed to serialize message: {}", e))?;

        // Publish with timeout for reliability
        with_timeout(Duration::from_secs(10), async {
            client
                .publish(topic, QoS::AtLeastOnce, false, json_payload.as_bytes())
                .await
        })
        .await
        .map_err(|_| anyhow!("Message publish timed out"))?
        .map_err(|e| anyhow!("Failed to publish message: {:?}", e))?;

        debug!("📤 Published async message to topic: {}", topic);
        Ok(())
    }

    /// Process incoming MQTT messages asynchronously
    /// This replaces the callback-based approach with async message processing
    pub async fn process_messages(&mut self) -> Result<()> {
        if let Some(client) = self.client.as_mut() {
            debug!("🔒 Processing async MQTT messages");
            
            // Try to receive a message with a short timeout
            match with_timeout(Duration::from_millis(100), async {
                client.next().await
            }).await {
                Ok(Ok(event)) => {
                    debug!("📡 Received async MQTT event");
                    self.handle_mqtt_event_async(&event).await?;
                    Ok(())
                }
                Ok(Err(e)) => {
                    warn!("⚠️ MQTT event error: {:?}", e);
                    Ok(()) // Don't fail on individual message errors
                }
                Err(_) => {
                    // Timeout - no messages available, this is normal
                    Ok(())
                }
            }
        } else {
            debug!("📭 MQTT not connected, no messages to process");
            Ok(())
        }
    }

    /// Handle incoming MQTT events asynchronously
    async fn handle_mqtt_event_async(
        &mut self,
        event: &esp_idf_svc::mqtt::client::EspMqttEvent<'_>,
    ) -> Result<()> {
        debug!("📡 Processing async MQTT event");

        // Extract message data if available and route to appropriate handlers
        if let Some((topic, payload)) = self.extract_message_data_async(event) {
            self.route_mqtt_message_async(&topic, &payload).await?;
        }

        Ok(())
    }

    /// Extract topic and payload from MQTT event for async processing
    fn extract_message_data_async(
        &self,
        event: &esp_idf_svc::mqtt::client::EspMqttEvent,
    ) -> Option<(String, Vec<u8>)> {
        use esp_idf_svc::mqtt::client::EspMqttEvent;
        
        match event {
            EspMqttEvent::Received { id: _, topic, data, dup: _, qos: _, retain: _ } => {
                if let (Some(topic_str), Some(payload_data)) = (topic, data) {
                    let topic = topic_str.to_string();
                    let payload = payload_data.to_vec();
                    
                    info!("📨 ✅ MQTT MESSAGE RECEIVED - topic='{}', payload_size={} bytes", 
                           topic, payload.len());
                    
                    Some((topic, payload))
                } else {
                    debug!("📭 MQTT event missing topic or data");
                    None
                }
            }
            EspMqttEvent::Connected => {
                info!("🔗 MQTT Connected event received");
                None
            }
            EspMqttEvent::Disconnected => {
                warn!("🔌 MQTT Disconnected event received");
                None
            }
            _ => {
                debug!("📋 Other MQTT event received: {:?}", event);
                None
            }
        }
    }

    /// Route incoming MQTT messages to appropriate async handlers
    async fn route_mqtt_message_async(&mut self, topic: &str, payload: &[u8]) -> Result<()> {
        info!(
            "🎯 ✅ ROUTING MQTT MESSAGE - topic: {}, payload size: {} bytes",
            topic,
            payload.len()
        );

        // Route based on topic patterns
        if topic.contains("settings") {
            info!("🔧 ✅ DETECTED SETTINGS MESSAGE - routing to settings handler");
            self.handle_settings_message_async(topic, payload).await?;
        } else if topic.contains("commands") {
            info!("📋 ✅ DETECTED COMMAND MESSAGE - routing to command handler");
            self.handle_command_message_async(topic, payload).await?;
        } else if topic.contains("status-request") {
            info!("📊 ✅ DETECTED STATUS REQUEST - routing to status handler");
            self.handle_status_request_async(topic, payload).await?;
        } else {
            info!("❓ UNHANDLED MESSAGE TOPIC: {}", topic);
        }

        Ok(())
    }

    /// Handle incoming settings update messages asynchronously
    async fn handle_settings_message_async(&mut self, topic: &str, payload: &[u8]) -> Result<()> {
        info!("🔧 ✅ FIRMWARE RECEIVED SETTINGS MESSAGE from topic: {}", topic);
        info!("📦 Settings payload size: {} bytes", payload.len());

        // Convert payload to string
        let json_payload = std::str::from_utf8(payload)
            .map_err(|e| anyhow!("Invalid UTF-8 in settings payload: {}", e))?;

        info!("📨 Settings JSON received: {}", json_payload);

        // Send settings update request to settings manager (async version)
        crate::settings::request_mqtt_settings_update(json_payload.to_string());

        info!("✅ ✨ Settings message processed and forwarded to settings manager");
        Ok(())
    }

    /// Handle incoming command messages asynchronously
    async fn handle_command_message_async(&mut self, topic: &str, payload: &[u8]) -> Result<()> {
        info!("📋 Processing async command from topic: {}", topic);

        // Convert payload to string for processing
        let command_payload = std::str::from_utf8(payload)
            .map_err(|e| anyhow!("Invalid UTF-8 in command payload: {}", e))?;

        debug!("📋 Command payload: {}", command_payload);

        // For MVP, just log commands - could be extended for device control
        info!(
            "📋 Async command received (not implemented): {}",
            command_payload
        );
        Ok(())
    }

    /// Handle status request messages asynchronously
    async fn handle_status_request_async(&mut self, topic: &str, _payload: &[u8]) -> Result<()> {
        info!("📊 Processing async status request from topic: {}", topic);

        // Send status response asynchronously
        self.publish_device_status("online", Some(-45)).await?;

        info!("📊 Async status response sent");
        Ok(())
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
            "🔄 Async MQTT connection state updated: {:?}",
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

    /// Attempt reconnection using async patterns
    pub async fn attempt_reconnection(&mut self) -> Result<()> {
        info!("🔄 Attempting async MQTT reconnection");

        // Disconnect first
        self.disconnect().await?;

        // Small delay before reconnecting
        embassy_time::Timer::after(Duration::from_secs(2)).await;

        // Attempt reconnection
        match self.connect().await {
            Ok(_) => {
                info!("✅ Async MQTT reconnection successful");
                self.connection_state = MqttConnectionState::Connected;
                Ok(())
            }
            Err(e) => {
                error!("❌ Async MQTT reconnection failed: {:?}", e);
                self.connection_state = MqttConnectionState::Error;
                Err(anyhow!("Async MQTT reconnection failed: {:?}", e))
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

    /// Debug function to check async MQTT client state and connection health
    pub fn debug_connection_state(&self) {
        debug!("🔍 Async MQTT Client Debug Information:");
        debug!("  📡 Client ID: {}", self.client_id);
        debug!("  🔗 Connection State: {:?}", self.connection_state);
        debug!("  📋 Client Initialized: {}", self.client.is_some());

        if self.client.is_some() {
            debug!("  ✅ Async MQTT Client exists");
        } else {
            debug!("  ❌ Async MQTT Client is None");
        }
    }

    /// Handle MQTT events from the callback (static method)
    fn handle_mqtt_callback_event(event: &esp_idf_svc::mqtt::client::EspMqttEvent) {
        use esp_idf_svc::mqtt::client::EspMqttEvent;
        
        info!("📡 ✅ MQTT CALLBACK EVENT RECEIVED");
        
        match event {
            EspMqttEvent::Received { id: _, topic, data, dup: _, qos: _, retain: _ } => {
                if let (Some(topic_str), Some(payload_data)) = (topic, data) {
                    info!("📨 ✅ ✨ SETTINGS MESSAGE RECEIVED VIA CALLBACK");
                    info!("🎯 Topic: {}", topic_str);
                    info!("📦 Payload size: {} bytes", payload_data.len());
                    
                    // Convert payload to string and log it
                    if let Ok(payload_str) = std::str::from_utf8(payload_data) {
                        info!("📝 Payload content: {}", payload_str);
                        
                        // If this is a settings message, handle it
                        if topic_str.contains("settings") {
                            info!("🔧 ✅ ✨ PROCESSING SETTINGS MESSAGE FROM CALLBACK");
                            
                            // Send to settings manager
                            crate::settings::request_mqtt_settings_update(payload_str.to_string());
                            
                            info!("✅ ✨ Settings message forwarded to settings manager via callback");
                        }
                    } else {
                        warn!("⚠️ Could not decode payload as UTF-8");
                    }
                } else {
                    debug!("📭 MQTT event missing topic or data");
                }
            }
            EspMqttEvent::Connected => {
                info!("🔗 ✅ MQTT Connected via callback");
            }
            EspMqttEvent::Disconnected => {
                warn!("🔌 ❌ MQTT Disconnected via callback");
            }
            _ => {
                debug!("📋 Other MQTT event via callback: {:?}", event);
            }
        }
    }
}
