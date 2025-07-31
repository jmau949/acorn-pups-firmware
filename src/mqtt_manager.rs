// Event-driven MQTT Manager using Embassy async runtime

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use log::{debug, error, info, warn};
use anyhow::{anyhow, Result};

use crate::mqtt_certificates::MqttCertificateStorage;
use crate::mqtt_client::{AwsIotMqttClient, MqttConnectionState};

const MQTT_MESSAGE_QUEUE_SIZE: usize = 32;
const CONNECTION_CHECK_INTERVAL_SECONDS: u64 = 60;

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

pub static MQTT_MESSAGE_CHANNEL: Channel<CriticalSectionRawMutex, MqttMessage, MQTT_MESSAGE_QUEUE_SIZE> = Channel::new();
pub static MQTT_EVENT_SIGNAL: Signal<CriticalSectionRawMutex, MqttManagerEvent> = Signal::new();

/// MQTT Manager - handles Embassy task coordination and message queuing
pub struct MqttManager {
    device_id: String,
    client: AwsIotMqttClient,
    cert_storage: Option<MqttCertificateStorage>,
    is_initialized: bool,
    subscriptions_completed: bool,
    subscription_in_progress: bool,
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
            subscriptions_completed: false,
            subscription_in_progress: false,
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

    /// Main MQTT management loop - fully event-driven using Embassy
    pub async fn run(&mut self) -> Result<()> {
        if !self.is_ready() {
            return Err(anyhow!("MQTT manager not initialized"));
        }

        info!("🚀 Starting event-driven MQTT manager");

        // Attempt initial connection
        if let Err(e) = self.attempt_connection().await {
            error!("❌ Initial MQTT connection failed: {}", e);
            crate::SYSTEM_EVENT_SIGNAL.signal(crate::SystemEvent::SystemError(format!(
                "MQTT connection failed: {}", e
            )));
            return Err(anyhow!("MQTT connection failed"));
        }

        // Event-driven loop - process one event at a time
        let mut last_health_check = embassy_time::Instant::now();
        
        loop {
            // Use select to handle different event sources
            match embassy_futures::select::select(
                MQTT_MESSAGE_CHANNEL.receive(),
                self.client.process_messages()
            ).await {
                embassy_futures::select::Either::First(message) => {
                    // Handle queued message
                    self.handle_mqtt_message(message).await;
                }
                embassy_futures::select::Either::Second(result) => {
                    // Handle MQTT event
                    if let Err(e) = result {
                        warn!("⚠️ Error processing MQTT events: {}", e);
                    }
                    
                    // Check if we need to subscribe after connection
                    if !self.subscriptions_completed && self.client.is_connected() {
                        info!("🔗 MQTT connected - initiating topic subscriptions");
                        self.handle_subscriptions().await;
                    }
                }
            }
            
            // Periodic health check
            let now = embassy_time::Instant::now();
            if now.duration_since(last_health_check) >= Duration::from_secs(CONNECTION_CHECK_INTERVAL_SECONDS) {
                self.check_health().await;
                last_health_check = now;
            }
        }
    }


    /// Periodic health check
    async fn check_health(&mut self) {
        // Check connection health
        match self.client.get_connection_status() {
            MqttConnectionState::Disconnected => {
                info!("🔄 MQTT disconnected, attempting reconnection");
                if let Err(e) = self.attempt_reconnection().await {
                    warn!("⚠️ Reconnection failed: {}", e);
                }
            }
            MqttConnectionState::Error => {
                warn!("❌ MQTT connection error detected");
                if let Err(e) = self.force_reconnection().await {
                    error!("❌ Failed to recover from connection error: {}", e);
                }
            }
            _ => {}
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


    /// Handle MQTT topic subscriptions after connection is confirmed
    async fn handle_subscriptions(&mut self) {
        // Only attempt subscriptions if connected, not already completed, and not already in progress
        if !self.subscriptions_completed && !self.subscription_in_progress && self.client.is_connected() {
            info!("🔗 Connection confirmed - starting topic subscriptions");
            self.subscription_in_progress = true;
            
            // Add small delay to let the connection stabilize before subscribing
            Timer::after(Duration::from_millis(500)).await;
            
            match self.client.subscribe_to_device_topics().await {
                Ok(_) => {
                    info!("✅ MQTT subscriptions initiated");
                    self.subscriptions_completed = true;
                    self.subscription_in_progress = false;
                }
                Err(e) => {
                    error!("❌ Failed to initiate MQTT subscriptions: {}", e);
                    self.subscriptions_completed = true; // Prevent retries
                    self.subscription_in_progress = false;
                }
            }
        }
    }



    /// Attempt initial MQTT connection with async operations and proper event handling
    async fn attempt_connection(&mut self) -> Result<()> {
        info!("🔌 Attempting async MQTT connection with event monitoring");

        match self.client.connect().await {
            Ok(_) => {
                info!("🔗 MQTT connection initiated successfully");
                // Connection establishment will be confirmed by event processing
                // Subscriptions will be handled separately in the main loop after connection is confirmed

                info!("✅ MQTT connection setup completed - waiting for Connected event");
                MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::Connected);

                Ok(())
            }
            Err(e) => {
                warn!("⚠️ Async MQTT connection failed: {}", e);
                MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::ConnectionError(e.to_string()));
                Err(e)
            }
        }
    }

    /// Attempt async MQTT reconnection with automatic retry
    async fn attempt_reconnection(&mut self) -> Result<()> {
        info!("🔄 Attempting async MQTT reconnection");

        // Reset subscription flags for reconnection
        self.subscriptions_completed = false;
        self.subscription_in_progress = false;

        match self.client.attempt_reconnection().await {
            Ok(_) => {
                info!("✅ Async MQTT reconnected successfully");
                MQTT_EVENT_SIGNAL.signal(MqttManagerEvent::Connected);
                Ok(())
            }
            Err(e) => {
                warn!("⚠️ Async MQTT reconnection failed: {}", e);
                Err(e)
            }
        }
    }

    /// Force reconnection by disconnecting and reconnecting using async operations
    async fn force_reconnection(&mut self) -> Result<()> {
        info!("🔄 Forcing async MQTT reconnection");

        // Reset subscription flags for reconnection
        self.subscriptions_completed = false;
        self.subscription_in_progress = false;

        // Disconnect first using async operations
        if let Err(e) = self.client.disconnect().await {
            warn!("⚠️ Error during async disconnect: {}", e);
        }

        // Small delay before reconnecting
        Timer::after(Duration::from_secs(2)).await;

        // Attempt connection using async operations
        self.attempt_connection().await
    }

    /// Get topic name for a message (for event reporting)
    fn get_message_topic(&self, message: &MqttMessage) -> String {
        let client_id = format!("acorn-receiver-{}", self.device_id);
        match message {
            MqttMessage::ButtonPress { .. } => {
                format!("acorn-pups/button-press/{}", client_id)
            }
            MqttMessage::DeviceStatus { .. } => {
                format!("acorn-pups/status-response/{}", client_id)
            }
            MqttMessage::VolumeChange { .. } => {
                format!("acorn-pups/status-response/{}", client_id)
            }
            _ => "system".to_string(),
        }
    }
}

// Public API functions for external modules to interact with async MQTT manager

/// Send button press message to async MQTT manager
pub async fn send_button_press(button_rf_id: String, battery_level: Option<u8>) -> Result<()> {
    let message = MqttMessage::ButtonPress {
        button_rf_id,
        battery_level,
    };

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            debug!("📤 Async button press message queued successfully");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue async button press message: {:?}", e);
            Err(anyhow!("Async message queue full or unavailable"))
        }
    }
}

/// Send device status update to async MQTT manager
pub async fn send_device_status(status: String, wifi_signal: Option<i32>) -> Result<()> {
    let message = MqttMessage::DeviceStatus {
        status,
        wifi_signal,
    };

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            debug!("📤 Async device status message queued successfully");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue async device status message: {:?}", e);
            Err(anyhow!("Async message queue full or unavailable"))
        }
    }
}

/// Send volume change notification to async MQTT manager
pub async fn send_volume_change(volume: u8, source: String) -> Result<()> {
    let message = MqttMessage::VolumeChange { volume, source };

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            debug!("📤 Async volume change message queued successfully");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue async volume change message: {:?}", e);
            Err(anyhow!("Async message queue full or unavailable"))
        }
    }
}

/// Request async MQTT connection
pub async fn request_connection() -> Result<()> {
    let message = MqttMessage::Connect;

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            info!("📤 Async MQTT connection request queued");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue async connection request: {:?}", e);
            Err(anyhow!("Async message queue full or unavailable"))
        }
    }
}

/// Request async MQTT disconnection
pub async fn request_disconnection() -> Result<()> {
    let message = MqttMessage::Disconnect;

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            info!("📤 Async MQTT disconnection request queued");
            Ok(())
        }
        Err(e) => {
            warn!("⚠️ Failed to queue async disconnection request: {:?}", e);
            Err(anyhow!("Async message queue full or unavailable"))
        }
    }
}

/// Force async MQTT reconnection
pub async fn force_reconnection() -> Result<()> {
    let message = MqttMessage::ForceReconnect;

    match MQTT_MESSAGE_CHANNEL.try_send(message) {
        Ok(_) => {
            info!("📤 Async MQTT force reconnection request queued");
            Ok(())
        }
        Err(e) => {
            warn!(
                "⚠️ Failed to queue async force reconnection request: {:?}",
                e
            );
            Err(anyhow!("Async message queue full or unavailable"))
        }
    }
}
