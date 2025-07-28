// MQTT Manager Module
// Embassy task coordination layer for MQTT operations with channel-based communication
// Integrates with existing signal system for seamless MQTT management

// Import Embassy synchronization primitives for task coordination
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;

// Import Embassy time utilities for periodic operations and timeouts
use embassy_time::{Duration, Timer};

// Import logging macros for debug output with consistent emoji prefixes
use log::{debug, error, info, warn};

// Import anyhow for error handling following existing patterns
use anyhow::{anyhow, Result};

// Import our MQTT client and certificate management modules
use crate::mqtt_certificates::MqttCertificateStorage;
use crate::mqtt_client::{AwsIotMqttClient, ConnectionStatus};

// Import existing system events for integration
// System events imported where needed

// MQTT manager configuration constants
const MQTT_MESSAGE_QUEUE_SIZE: usize = 32; // Channel capacity for message queue
const HEARTBEAT_INTERVAL_SECONDS: u64 = 300; // 5 minutes
const CONNECTION_CHECK_INTERVAL_SECONDS: u64 = 30; // 30 seconds
const MESSAGE_TIMEOUT_SECONDS: u64 = 10; // Timeout for message operations

// MQTT message types for internal communication
#[derive(Debug, Clone)]
pub enum MqttMessage {
    ButtonPress {
        button_rf_id: String,
        battery_level: Option<u8>,
    },
    DeviceStatus {
        status: String,
        wifi_signal: Option<i32>,
    },
    VolumeChange {
        volume: u8,
        source: String,
    },
    Heartbeat,
    Connect,
    Disconnect,
    ForceReconnect,
}

// MQTT manager events for external communication
#[derive(Debug, Clone)]
pub enum MqttManagerEvent {
    Connected,
    Disconnected,
    ConnectionError(String),
    MessagePublished(String), // topic name
    MessageFailed(String),    // error message
    CertificatesLoaded,
    CertificatesError(String),
}

// Global MQTT message channel for task communication
// Using CriticalSectionRawMutex for interrupt-safe access
pub static MQTT_MESSAGE_CHANNEL: Channel<
    CriticalSectionRawMutex,
    MqttMessage,
    MQTT_MESSAGE_QUEUE_SIZE,
> = Channel::new();

// Global MQTT manager event signal for external coordination
pub static MQTT_EVENT_SIGNAL: Signal<CriticalSectionRawMutex, MqttManagerEvent> = Signal::new();

/// MQTT Manager - handles Embassy task coordination and message queuing
pub struct MqttManager {
    device_id: String,
    client: AwsIotMqttClient,
    cert_storage: Option<MqttCertificateStorage>,
    is_initialized: bool,
    last_heartbeat: Option<embassy_time::Instant>,
    last_connection_check: Option<embassy_time::Instant>,
}

impl MqttManager {
    /// Create new MQTT manager with device configuration
    pub fn new(device_id: String) -> Self {
        info!("🔌 Creating MQTT manager for device: {}", device_id);

        let client = AwsIotMqttClient::new(device_id.clone());

        Self {
            device_id,
            client,
            cert_storage: None,
            is_initialized: false,
            last_heartbeat: None,
            last_connection_check: None,
        }
    }

    /// Initialize MQTT manager with certificate storage
    pub async fn initialize(&mut self, cert_storage: MqttCertificateStorage) -> Result<()> {
        info!("🔐 Initializing MQTT manager with certificate storage");

        self.cert_storage = Some(cert_storage);

        // Load certificates with optimized buffer sizing and initialize MQTT client
        if let Some(ref mut storage) = self.cert_storage {
            match self.client.initialize_with_certificates(storage).await {
                Ok(_) => {
                    self.is_initialized = true;
                    info!("✅ MQTT manager initialized successfully");

                    // Signal that certificates are loaded
                    MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::CertificatesLoaded);

                    Ok(())
                }
                Err(e) => {
                    let error_msg = format!("Failed to initialize MQTT client: {}", e);
                    error!("❌ {}", error_msg);

                    // Signal certificate error
                    MQTT_EVENT_SIGNAL
                        .signal(MqttManagerEvent::CertificatesError(error_msg.clone()));

                    Err(anyhow!(error_msg))
                }
            }
        } else {
            Err(anyhow!("Certificate storage not available"))
        }
    }

    /// Check if MQTT manager is ready for operations
    pub fn is_ready(&self) -> bool {
        self.is_initialized && self.cert_storage.is_some()
    }

    /// Main MQTT management loop - processes messages and handles connection health
    pub async fn run(&mut self) -> Result<()> {
        if !self.is_ready() {
            return Err(anyhow!("MQTT manager not initialized"));
        }

        info!("🚀 Starting MQTT manager main loop");

        // Attempt initial connection with detailed error handling
        match self.attempt_connection().await {
            Ok(_) => {
                info!("✅ Initial MQTT connection successful - entering main loop");
            }
            Err(e) => {
                error!("❌ Initial MQTT connection failed: {}", e);
                error!("💥 MQTT connection is critical - triggering factory reset");
                error!("🔄 Device will reset to BLE provisioning mode");

                // Signal MQTT failure to trigger factory reset
                crate::SYSTEM_EVENT_SIGNAL.signal(crate::SystemEvent::SystemError(format!(
                    "MQTT connection failed: {}",
                    e
                )));

                // Give time for the signal to be processed
                Timer::after(Duration::from_secs(2)).await;

                return Err(anyhow!("MQTT connection failed - factory reset triggered"));
            }
        }

        loop {
            // Process each task in sequence with proper async coordination
            // This avoids multiple mutable borrow issues with select!
            self.process_message_queue().await;
            self.check_connection_health().await;
            self.handle_periodic_heartbeat().await;
            self.handle_system_events().await;
        }
    }

    /// Process incoming messages from the queue
    async fn process_message_queue(&mut self) {
        // Non-blocking message processing to avoid deadlocks
        match MQTT_MESSAGE_CHANNEL.try_receive() {
            Ok(message) => {
                self.handle_mqtt_message(message).await;
            }
            Err(_) => {
                // No messages available, yield to other tasks
                Timer::after(Duration::from_millis(10)).await;
            }
        }
    }

    /// Handle individual MQTT messages
    async fn handle_mqtt_message(&mut self, message: MqttMessage) {
        debug!("📬 Processing MQTT message: {:?}", message);

        // Clone message for topic generation before moving it
        let message_copy = message.clone();

        let result = match message {
            MqttMessage::ButtonPress {
                button_rf_id,
                battery_level,
            } => {
                self.client
                    .publish_button_press(&button_rf_id, battery_level)
                    .await
            }

            MqttMessage::DeviceStatus {
                status,
                wifi_signal,
            } => {
                self.client
                    .publish_device_status(&status, wifi_signal)
                    .await
            }

            MqttMessage::VolumeChange { volume, source } => {
                self.client.publish_volume_change(volume, &source).await
            }

            MqttMessage::Heartbeat => self.client.publish_heartbeat().await,

            MqttMessage::Connect => self.attempt_connection().await,

            MqttMessage::Disconnect => self.client.disconnect().await,

            MqttMessage::ForceReconnect => self.force_reconnection().await,
        };

        // Handle message processing results
        match result {
            Ok(_) => {
                debug!("✅ MQTT message processed successfully");
                let topic = self.get_message_topic(&message_copy);
                MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::MessagePublished(topic));
            }
            Err(e) => {
                warn!("⚠️ Failed to process MQTT message: {}", e);
                MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::MessageFailed(e.to_string()));

                // If message failed due to connection issues, attempt reconnection
                if !self.client.is_connected() {
                    if let Err(reconnect_error) = self.attempt_reconnection().await {
                        warn!("⚠️ Reconnection attempt failed: {}", reconnect_error);
                    }
                }
            }
        }
    }

    /// Check connection health and attempt reconnection if needed
    async fn check_connection_health(&mut self) {
        let now = embassy_time::Instant::now();

        // Check if it's time for a connection health check
        if let Some(last_check) = self.last_connection_check {
            if now.duration_since(last_check)
                < Duration::from_secs(CONNECTION_CHECK_INTERVAL_SECONDS)
            {
                // Not time for check yet, yield to avoid busy waiting
                Timer::after(Duration::from_millis(100)).await;
                return;
            }
        }

        self.last_connection_check = Some(now);

        // Check current connection status
        let status = self.client.get_connection_status();

        match status {
            ConnectionStatus::Connected => {
                debug!("💚 MQTT connection healthy");

                // Process any pending messages
                if let Err(e) = self.client.process_messages().await {
                    warn!("⚠️ Message processing error: {}", e);
                }
            }

            ConnectionStatus::Disconnected => {
                info!("🔄 MQTT disconnected, attempting reconnection");
                if let Err(e) = self.attempt_reconnection().await {
                    warn!("⚠️ Reconnection failed: {}", e);
                }
            }

            ConnectionStatus::Connecting => {
                debug!("🔄 MQTT connection in progress");
            }

            ConnectionStatus::Error(ref error) => {
                warn!("❌ MQTT connection error: {}", error);
                MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::ConnectionError(error.clone()));

                // Attempt recovery
                if let Err(e) = self.force_reconnection().await {
                    error!("❌ Failed to recover from connection error: {}", e);
                }
            }
        }
    }

    /// Handle periodic heartbeat messages
    async fn handle_periodic_heartbeat(&mut self) {
        let now = embassy_time::Instant::now();

        // Check if it's time for a heartbeat
        if let Some(last_heartbeat) = self.last_heartbeat {
            if now.duration_since(last_heartbeat) < Duration::from_secs(HEARTBEAT_INTERVAL_SECONDS)
            {
                // Not time for heartbeat yet, yield to avoid busy waiting
                Timer::after(Duration::from_millis(100)).await;
                return;
            }
        }

        self.last_heartbeat = Some(now);

        // Send heartbeat if connected
        if self.client.is_connected() {
            debug!("💓 Sending periodic heartbeat");

            // Queue heartbeat message instead of blocking
            if let Err(e) = MQTT_MESSAGE_CHANNEL.try_send(MqttMessage::Heartbeat) {
                warn!("⚠️ Failed to queue heartbeat message: {:?}", e);
            }
        }
    }

    /// Handle system events from the main application
    async fn handle_system_events(&mut self) {
        // Check for system events that might affect MQTT operations
        // This is a non-blocking check to integrate with existing signal system

        // For now, we just yield to allow other tasks to signal events
        Timer::after(Duration::from_millis(10)).await;
    }

    /// Attempt initial MQTT connection
    async fn attempt_connection(&mut self) -> Result<()> {
        info!("🔌 Attempting MQTT connection");

        match self.client.connect().await {
            Ok(_) => {
                info!("✅ MQTT connected successfully");
                MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::Connected);

                // Send initial device status
                let _ = MQTT_MESSAGE_CHANNEL.try_send(MqttMessage::DeviceStatus {
                    status: "online".to_string(),
                    wifi_signal: Some(-45), // TODO: Get actual WiFi signal
                });

                Ok(())
            }
            Err(e) => {
                warn!("⚠️ MQTT connection failed: {}", e);
                MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::ConnectionError(e.to_string()));
                Err(e)
            }
        }
    }

    /// Attempt MQTT reconnection with automatic retry
    async fn attempt_reconnection(&mut self) -> Result<()> {
        info!("🔄 Attempting MQTT reconnection");

        match self.client.attempt_reconnection().await {
            Ok(_) => {
                info!("✅ MQTT reconnected successfully");
                MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::Connected);
                Ok(())
            }
            Err(e) => {
                warn!("⚠️ MQTT reconnection failed: {}", e);
                Err(e)
            }
        }
    }

    /// Force reconnection by disconnecting and reconnecting
    async fn force_reconnection(&mut self) -> Result<()> {
        info!("🔄 Forcing MQTT reconnection");

        // Disconnect first
        if let Err(e) = self.client.disconnect().await {
            warn!("⚠️ Error during disconnect: {}", e);
        }

        // Small delay before reconnecting
        Timer::after(Duration::from_secs(2)).await;

        // Attempt connection
        self.attempt_connection().await
    }

    /// Get topic name for a message (for event reporting)
    fn get_message_topic(&self, message: &MqttMessage) -> String {
        match message {
            MqttMessage::ButtonPress { .. } => {
                format!("acorn-pups/button-press/{}", self.device_id)
            }
            MqttMessage::DeviceStatus { .. } => format!("acorn-pups/status/{}", self.device_id),
            MqttMessage::VolumeChange { .. } => format!("acorn-pups/status/{}", self.device_id),
            MqttMessage::Heartbeat => format!("acorn-pups/heartbeat/{}", self.device_id),
            _ => "system".to_string(),
        }
    }
}

// Public API functions for external modules to interact with MQTT manager

/// Send button press message to MQTT manager
pub async fn send_button_press(button_rf_id: String, battery_level: Option<u8>) -> Result<()> {
    let message = MqttMessage::ButtonPress {
        button_rf_id,
        battery_level,
    };

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            debug!("📤 Button press message queued successfully");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue button press message: {:?}", e);
            Err(anyhow!("Message queue full or unavailable"))
        }
    }
}

/// Send device status update to MQTT manager
pub async fn send_device_status(status: String, wifi_signal: Option<i32>) -> Result<()> {
    let message = MqttMessage::DeviceStatus {
        status,
        wifi_signal,
    };

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            debug!("📤 Device status message queued successfully");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue device status message: {:?}", e);
            Err(anyhow!("Message queue full or unavailable"))
        }
    }
}

/// Send volume change notification to MQTT manager
pub async fn send_volume_change(volume: u8, source: String) -> Result<()> {
    let message = MqttMessage::VolumeChange { volume, source };

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            debug!("📤 Volume change message queued successfully");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue volume change message: {:?}", e);
            Err(anyhow!("Message queue full or unavailable"))
        }
    }
}

/// Request MQTT connection
pub async fn request_connection() -> Result<()> {
    let message = MqttMessage::Connect;

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            info!("📤 MQTT connection request queued");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue connection request: {:?}", e);
            Err(anyhow!("Message queue full or unavailable"))
        }
    }
}

/// Request MQTT disconnection
pub async fn request_disconnection() -> Result<()> {
    let message = MqttMessage::Disconnect;

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            info!("📤 MQTT disconnection request queued");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue disconnection request: {:?}", e);
            Err(anyhow!("Message queue full or unavailable"))
        }
    }
}

/// Force MQTT reconnection
pub async fn force_reconnection() -> Result<()> {
    let message = MqttMessage::ForceReconnect;

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            info!("📤 MQTT force reconnection request queued");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue force reconnection request: {:?}", e);
            Err(anyhow!("Message queue full or unavailable"))
        }
    }
}
