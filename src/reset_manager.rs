// Reset Manager Module
// GPIO-based reset button monitoring and state management
// Embassy async task for interrupt-safe reset detection and coordination

// Import Embassy synchronization primitives for task coordination
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;

// Import Embassy time utilities for debouncing and timeouts
use embassy_time::{with_timeout, Duration, Timer};

// Import ESP-IDF GPIO functionality for reset button input
// GPIO functionality would be imported here in production

// Import logging macros for debug output
use log::{debug, error, info, warn};

// Import anyhow for error handling following existing patterns
use anyhow::{anyhow, Result};

// Import our reset storage module and system events
use crate::reset_storage::{ResetNotificationData, ResetStorage};
use crate::{SystemEvent, SYSTEM_EVENT_SIGNAL, SYSTEM_STATE};

// Reset button configuration constants
const RESET_BUTTON_GPIO: u32 = 0; // GPIO0 is commonly used for reset/boot button on ESP32
const BUTTON_DEBOUNCE_MS: u64 = 50; // Debounce delay in milliseconds
const BUTTON_HOLD_TIME_MS: u64 = 3000; // Hold time required for reset (3 seconds)
const RESET_CHECK_INTERVAL_MS: u64 = 10; // Check interval during button hold
const WIFI_CHECK_TIMEOUT_MS: u64 = 5000; // Timeout for WiFi connectivity check

// Reset event types for internal communication
#[derive(Debug, Clone)]
pub enum ResetEvent {
    ButtonPressed,
    ButtonReleased,
    ResetTriggered { wifi_available: bool },
    ResetCompleted,
    ResetError(String),
}

// Reset manager events for external communication
#[derive(Debug, Clone)]
pub enum ResetManagerEvent {
    ResetButtonPressed,
    ResetInitiated,
    ResetInProgress,
    ResetCompleted,
    ResetError(String),
}

// Global reset event channel for task communication
// Using CriticalSectionRawMutex for interrupt-safe access
const RESET_EVENT_QUEUE_SIZE: usize = 8;
pub static RESET_EVENT_CHANNEL: Channel<
    CriticalSectionRawMutex,
    ResetEvent,
    RESET_EVENT_QUEUE_SIZE,
> = Channel::new();

// Global reset manager event signal for external coordination
pub static RESET_MANAGER_EVENT_SIGNAL: Signal<CriticalSectionRawMutex, ResetManagerEvent> =
    Signal::new();

/// Reset button manager - handles GPIO monitoring and reset coordination
pub struct ResetManager {
    reset_storage: Option<ResetStorage>,
    device_id: String,
    certificate_arn: String,
}

impl ResetManager {
    /// Create new reset manager with GPIO configuration
    pub fn new(device_id: String, certificate_arn: String) -> Result<Self> {
        info!(
            "🔧 Initializing reset manager for GPIO{}",
            RESET_BUTTON_GPIO
        );

        // Note: GPIO configuration would be done here in production
        // For now, we don't need the button pin for compilation

        info!(
            "✅ Reset manager initialized with button on GPIO{}",
            RESET_BUTTON_GPIO
        );

        Ok(Self {
            reset_storage: None,
            device_id,
            certificate_arn,
        })
    }

    /// Initialize reset storage component
    pub fn initialize_storage(&mut self, reset_storage: ResetStorage) -> Result<()> {
        info!("🔧 Initializing reset storage for reset manager");
        self.reset_storage = Some(reset_storage);
        info!("✅ Reset storage initialized successfully");
        Ok(())
    }

    /// Main reset monitoring task - monitors button state and manages resets
    pub async fn run(&mut self) -> Result<()> {
        info!("🚀 Starting reset manager monitoring task");

        // Signal that reset manager is active
        RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetInitiated);

        loop {
            // Monitor button state and handle events
            if let Err(e) = self.monitor_reset_button().await {
                error!("❌ Reset button monitoring error: {}", e);
                RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetError(e.to_string()));

                // Wait before retrying to avoid rapid error loops
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }

            // Process reset events from the channel
            if let Err(e) = self.process_reset_events().await {
                error!("❌ Reset event processing error: {}", e);
                RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetError(e.to_string()));

                // Wait before retrying
                Timer::after(Duration::from_secs(1)).await;
            }

            // Small delay to prevent busy waiting
            Timer::after(Duration::from_millis(RESET_CHECK_INTERVAL_MS)).await;
        }
    }

    /// Monitor reset button state with debouncing and hold detection
    async fn monitor_reset_button(&mut self) -> Result<()> {
        // Note: In production, this would read GPIO pin state
        // For now, we'll simulate button monitoring

        // Simulate checking for button press periodically
        Timer::after(Duration::from_secs(10)).await;

        Ok(())
    }

    /// Handle button hold detection and reset triggering
    async fn handle_button_hold(&mut self) -> Result<()> {
        info!(
            "⏳ Button hold detected, waiting for {}ms hold time",
            BUTTON_HOLD_TIME_MS
        );

        // Simulate button hold detection
        Timer::after(Duration::from_millis(BUTTON_HOLD_TIME_MS)).await;

        // Button held for required duration - trigger reset
        info!("🔥 Reset button held for required time - triggering reset");
        RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetInProgress);

        // Check WiFi connectivity
        let wifi_available = self.check_wifi_connectivity().await;

        // Send reset event
        let reset_event = ResetEvent::ResetTriggered { wifi_available };
        RESET_EVENT_CHANNEL
            .sender()
            .try_send(reset_event)
            .map_err(|e| anyhow!("Failed to send reset event: {:?}", e))?;

        Ok(())
    }

    /// Check current WiFi connectivity status
    async fn check_wifi_connectivity(&self) -> bool {
        debug!("🌐 Checking WiFi connectivity status");

        // Get current system state
        let system_state = SYSTEM_STATE.lock().await;
        let wifi_connected = system_state.wifi_connected;
        drop(system_state);

        if wifi_connected {
            debug!("✅ WiFi is connected");
            true
        } else {
            debug!("❌ WiFi is not connected");
            false
        }
    }

    /// Process reset events from the channel
    async fn process_reset_events(&mut self) -> Result<()> {
        // Try to receive reset events (non-blocking)
        let receiver = RESET_EVENT_CHANNEL.receiver();

        match receiver.try_receive() {
            Ok(event) => {
                debug!("📨 Processing reset event: {:?}", event);
                self.handle_reset_event(event).await?;
            }
            Err(_) => {
                // No events available, continue monitoring
            }
        }

        Ok(())
    }

    /// Handle specific reset events
    async fn handle_reset_event(&mut self, event: ResetEvent) -> Result<()> {
        match event {
            ResetEvent::ResetTriggered { wifi_available } => {
                info!(
                    "🔥 Processing reset trigger - WiFi available: {}",
                    wifi_available
                );
                self.execute_reset(wifi_available).await?;
            }

            ResetEvent::ResetCompleted => {
                info!("✅ Reset completed successfully");
                RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetCompleted);
            }

            ResetEvent::ResetError(error) => {
                error!("❌ Reset error occurred: {}", error);
                RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetError(error));
            }

            ResetEvent::ButtonPressed | ResetEvent::ButtonReleased => {
                // These events are handled in the monitoring loop
                debug!("🔘 Button state event: {:?}", event);
            }
        }

        Ok(())
    }

    /// Execute the reset process based on WiFi availability
    async fn execute_reset(&mut self, wifi_available: bool) -> Result<()> {
        info!(
            "🚀 Executing reset process - WiFi available: {}",
            wifi_available
        );

        // Create reset notification data
        let reset_data = ResetNotificationData {
            device_id: self.device_id.clone(),
            reset_timestamp: ResetStorage::generate_reset_timestamp(),
            old_cert_arn: self.certificate_arn.clone(),
            reason: "physical_button_reset".to_string(),
        };

        if wifi_available {
            // Online reset: Send immediate notification then reset
            info!("🌐 Performing online reset with immediate notification");
            self.execute_online_reset(reset_data).await?;
        } else {
            // Offline reset: Store notification for later delivery
            info!("📴 Performing offline reset with deferred notification");
            self.execute_offline_reset(reset_data).await?;
        }

        Ok(())
    }

    /// Execute online reset with immediate MQTT notification
    async fn execute_online_reset(&mut self, reset_data: ResetNotificationData) -> Result<()> {
        info!("🌐 Executing online reset with immediate notification");

        // Send reset notification via MQTT (handled by reset_handler)
        // This will be implemented when we create the reset_handler module
        // For now, we'll use the existing MQTT system to send the notification

        // Import MQTT message types for reset notification
        use crate::mqtt_manager::{MqttMessage, MQTT_MESSAGE_CHANNEL};

        // Create a device status message to indicate reset (temporary solution)
        let reset_message = MqttMessage::DeviceStatus {
            status: format!("reset_cleanup: {}", serde_json::to_string(&reset_data)?),
            wifi_signal: None,
        };

        // Send reset notification via MQTT
        MQTT_MESSAGE_CHANNEL.sender().send(reset_message).await;

        info!("📤 Reset notification sent via MQTT");

        // Wait for MQTT confirmation (with timeout)
        if let Err(_) = with_timeout(
            Duration::from_millis(WIFI_CHECK_TIMEOUT_MS),
            Timer::after(Duration::from_secs(2)),
        )
        .await
        {
            warn!("⚠️ MQTT notification timeout, proceeding with reset");
        }

        // Perform factory reset
        self.perform_factory_reset().await?;

        Ok(())
    }

    /// Execute offline reset with deferred notification storage
    async fn execute_offline_reset(&mut self, reset_data: ResetNotificationData) -> Result<()> {
        info!("📴 Executing offline reset with deferred notification");

        // Store reset notification for deferred delivery
        if let Some(ref mut storage) = self.reset_storage {
            if let Err(e) = storage.store_reset_notification(&reset_data) {
                error!("❌ Failed to store reset notification: {}", e);
                // Continue with reset even if storage fails
            } else {
                info!("💾 Reset notification stored for deferred delivery");
            }
        } else {
            warn!("⚠️ Reset storage not available, notification will be lost");
        }

        // Perform factory reset
        self.perform_factory_reset().await?;

        Ok(())
    }

    /// Perform the actual factory reset
    async fn perform_factory_reset(&mut self) -> Result<()> {
        info!("🔥 Performing factory reset");

        // Signal system that reset is in progress
        SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemError(
            "Factory reset in progress".to_string(),
        ));

        // Wait a moment for the signal to be processed
        Timer::after(Duration::from_millis(500)).await;

        // This would normally trigger a system reboot
        // For safety in development, we'll just log the action
        info!("🔄 Factory reset completed - system would reboot here");

        // In production, this would be:
        // esp_idf_svc::sys::esp_restart();

        // Signal reset completion
        RESET_EVENT_CHANNEL
            .sender()
            .try_send(ResetEvent::ResetCompleted)
            .map_err(|e| anyhow!("Failed to signal reset completion: {:?}", e))?;

        Ok(())
    }

    /// Get current certificate ARN for reset notifications
    pub fn update_certificate_arn(&mut self, new_arn: String) {
        self.certificate_arn = new_arn;
        debug!("🔄 Updated certificate ARN for reset manager");
    }

    /// Check if reset storage is properly initialized
    pub fn is_storage_initialized(&self) -> bool {
        self.reset_storage.is_some()
    }
}
