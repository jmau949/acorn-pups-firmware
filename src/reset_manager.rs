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
use esp_idf_svc::hal::gpio::{Gpio0, Input, PinDriver, Pull};

// Import logging macros for debug output
use log::{debug, error, info, warn};

// Import anyhow for error handling following existing patterns
use anyhow::{anyhow, Result};

// Import atomic operations for reset state management
use core::sync::atomic::{AtomicBool, Ordering};

// Import our reset storage module and system events
use crate::reset_storage::{ResetNotificationData, ResetStorage};
use crate::SYSTEM_STATE;

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
    ResetTriggered {
        wifi_available: bool,
        reset_data: ResetNotificationData,
    },
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

// Atomic reset state to prevent concurrent resets
static RESET_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Reset button manager - handles GPIO monitoring and reset coordination
pub struct ResetManager {
    reset_button: PinDriver<'static, Gpio0, Input>,
    reset_storage: Option<ResetStorage>,
    device_id: String,
    certificate_arn: String,
}

impl ResetManager {
    /// Create new reset manager with GPIO configuration
    pub fn new(device_id: String, certificate_arn: String, gpio0: Gpio0) -> Result<Self> {
        info!(
            "🔧 Initializing reset manager for GPIO{}",
            RESET_BUTTON_GPIO
        );

        // Configure reset button GPIO as input with internal pull-up
        // Pull-up means button press will read as LOW (0)
        let mut reset_button = PinDriver::input(gpio0)
            .map_err(|e| anyhow!("Failed to configure reset button GPIO: {}", e))?;

        reset_button
            .set_pull(Pull::Up)
            .map_err(|e| anyhow!("Failed to set pull-up on reset button GPIO: {}", e))?;

        info!(
            "✅ Reset manager initialized with button on GPIO{}",
            RESET_BUTTON_GPIO
        );

        Ok(Self {
            reset_button,
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
        // Read current button state (LOW = pressed due to pull-up)
        let button_pressed = self.reset_button.is_low();

        if button_pressed {
            debug!("🔘 Reset button pressed, starting hold detection");
            RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetButtonPressed);

            // Debounce the button press
            Timer::after(Duration::from_millis(BUTTON_DEBOUNCE_MS)).await;

            // Verify button is still pressed after debounce
            if self.reset_button.is_low() {
                // Start hold time measurement
                if let Err(e) = self.handle_button_hold().await {
                    warn!("⚠️ Button hold handling failed: {}", e);
                    return Err(e);
                }
            } else {
                debug!("🔘 Button press was too short (bounce)");
            }
        }

        // Small delay to prevent busy waiting
        Timer::after(Duration::from_millis(RESET_CHECK_INTERVAL_MS)).await;
        Ok(())
    }

    /// Handle button hold detection and reset triggering
    async fn handle_button_hold(&mut self) -> Result<()> {
        let start_time = embassy_time::Instant::now();
        let hold_duration = Duration::from_millis(BUTTON_HOLD_TIME_MS);

        info!(
            "⏳ Button hold detected, waiting for {}ms hold time",
            BUTTON_HOLD_TIME_MS
        );

        // Monitor button state during hold period
        while start_time.elapsed() < hold_duration {
            // Check if button was released
            if self.reset_button.is_high() {
                debug!("🔘 Button released before hold time completed");
                return Ok(());
            }

            Timer::after(Duration::from_millis(RESET_CHECK_INTERVAL_MS)).await;
        }

        // Button held for required duration - trigger reset
        info!("🔥 Reset button held for required time - triggering reset");

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
                // Reset completion is now handled by reset_handler directly
            }

            ResetEvent::ResetError(error) => {
                error!("❌ Reset error occurred: {}", error);
                // Reset errors are now handled by reset_handler directly
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
        // Check if reset is already in progress to prevent concurrent resets
        if RESET_IN_PROGRESS.swap(true, Ordering::SeqCst) {
            warn!("⚠️ Reset already in progress, ignoring duplicate request");
            return Ok(());
        }

        info!(
            "🚀 Triggering reset process - WiFi available: {}",
            wifi_available
        );

        // Create reset notification data
        let reset_data = ResetNotificationData {
            device_id: self.device_id.clone(),
            reset_timestamp: ResetStorage::generate_reset_timestamp(),
            old_cert_arn: self.certificate_arn.clone(),
            reason: "physical_button_reset".to_string(),
        };

        // Signal that reset is triggered - delegate execution to reset_handler
        RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetTriggered {
            wifi_available,
            reset_data,
        });

        info!("📡 Reset trigger signal sent to reset handler");

        // Reset manager's job is done - reset_handler will take over
        // Clear reset in progress flag will be handled by reset_handler
        RESET_IN_PROGRESS.store(false, Ordering::SeqCst);

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
