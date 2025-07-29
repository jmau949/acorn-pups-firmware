// Reset Manager Module
// GPIO-based reset button monitoring and state management
// Embassy async task for interrupt-safe reset detection and coordination

// Import Embassy synchronization primitives for task coordination
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;

// Import Embassy time utilities for debouncing and timeouts
use embassy_time::{Duration, Timer};

// Import ESP-IDF GPIO functionality for reset button input
use esp_idf_svc::hal::gpio::{Gpio0, Input, PinDriver, Pull};

// Import logging macros for debug output
use log::{debug, error, info, warn};

// Import anyhow for error handling following existing patterns
use anyhow::{anyhow, Result};

// Import atomic operations for reset state management
use core::sync::atomic::{AtomicBool, Ordering};

// Import our reset storage module and system events
// Note: SYSTEM_STATE import removed - no longer used in simplified reset architecture

// Import Serde traits for JSON serialization
use serde::{Deserialize, Serialize};

/// Reset notification data structure for event communication
/// Used to pass reset information between reset manager and reset handler
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetNotificationData {
    pub device_id: String,
    pub reset_timestamp: String, // ISO 8601 format
    pub old_cert_arn: String,
    pub reason: String,
}

/// Get current timestamp in proper ISO 8601 format using chrono
fn get_iso8601_timestamp() -> String {
    use chrono::Utc;

    // Use proper RFC3339/ISO8601 timestamp generation
    // This handles timezones, leap years, calendar correctness, etc.
    Utc::now().to_rfc3339()
}

// Reset button configuration constants
const RESET_BUTTON_GPIO: u32 = 0; // GPIO0 is commonly used for reset/boot button on ESP32
const BUTTON_DEBOUNCE_MS: u64 = 50; // Debounce delay in milliseconds
const BUTTON_HOLD_TIME_MS: u64 = 3000; // Hold time required for reset (3 seconds)
const RESET_CHECK_INTERVAL_MS: u64 = 10; // Check interval during button hold
                                         // Note: WIFI_CHECK_TIMEOUT_MS removed - no WiFi checking needed in new architecture

// Reset event types for internal communication (simplified)
#[derive(Debug, Clone)]
pub enum ResetEvent {
    ResetTriggered, // Physical reset button triggered (no WiFi distinction needed)
}

// Reset manager events for external communication (simplified)
#[derive(Debug, Clone)]
pub enum ResetManagerEvent {
    ResetButtonPressed, // Physical button was pressed
    ResetTriggered {
        // Reset process initiated (delegated to reset_handler)
        reset_data: ResetNotificationData,
    },
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

/// Reset button manager - handles GPIO monitoring and tiered recovery
pub struct ResetManager {
    reset_button: PinDriver<'static, Gpio0, Input>,
    device_id: String,
    certificate_arn: String,
    // Tiered recovery state
    recovery_attempts: u32,
    last_recovery_attempt: Option<embassy_time::Instant>,
}

impl ResetManager {
    /// Create new reset manager with GPIO button monitoring and tiered recovery
    pub fn new(
        reset_button: PinDriver<'static, Gpio0, Input>,
        device_id: String,
        certificate_arn: String,
    ) -> Self {
        info!("🔧 Initializing reset manager with tiered recovery approach");
        info!("📋 Device ID: {}", device_id);

        Self {
            reset_button,
            device_id,
            certificate_arn,
            recovery_attempts: 0,
            last_recovery_attempt: None,
        }
    }

    // Note: initialize_storage removed - reset manager no longer needs storage

    /// Main reset monitoring task - monitors button state and manages resets
    pub async fn run(&mut self) -> Result<()> {
        info!("🚀 Starting reset manager monitoring task");

        // Signal that reset manager is active
        info!("🎯 Reset manager monitoring started");

        loop {
            // Monitor button state and handle events
            if let Err(e) = self.monitor_reset_button().await {
                error!("❌ Reset button monitoring error: {}", e);
                // Error handling simplified - just log and retry
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }

            // Process reset events from the channel
            if let Err(e) = self.process_reset_events().await {
                error!("❌ Reset event processing error: {}", e);
                // Error handling simplified - just log and retry
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

        // Send reset event (no WiFi check needed in new architecture)
        let reset_event = ResetEvent::ResetTriggered;
        RESET_EVENT_CHANNEL
            .sender()
            .try_send(reset_event)
            .map_err(|e| anyhow!("Failed to send reset event: {:?}", e))?;

        Ok(())
    }

    // Note: check_wifi_connectivity method removed - no WiFi checking needed in new architecture

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
            ResetEvent::ResetTriggered => {
                info!("🔥 Processing reset trigger");
                self.execute_reset().await?;
            }
        }

        Ok(())
    }

    /// Execute the reset process with tiered recovery approach
    async fn execute_reset(&mut self) -> Result<()> {
        // Check if reset is already in progress to prevent concurrent resets
        if RESET_IN_PROGRESS.swap(true, Ordering::SeqCst) {
            warn!("⚠️ Reset already in progress, ignoring duplicate request");
            return Ok(());
        }

        info!("🚀 Starting tiered recovery process");

        // Tier 1: Graceful Recovery (No Reset)
        // Check if we should attempt graceful recovery first
        let should_attempt_recovery = self.recovery_attempts < 3; // Max 3 recovery attempts

        if should_attempt_recovery {
            self.recovery_attempts += 1;
            self.last_recovery_attempt = Some(embassy_time::Instant::now());

            info!(
                "🔄 Tier 1: Attempting graceful recovery (attempt {})",
                self.recovery_attempts
            );

            // Calculate exponential backoff delay
            let base_delay_secs = match self.recovery_attempts {
                1 => 30,  // 30 seconds
                2 => 60,  // 1 minute
                3 => 120, // 2 minutes
                _ => 300, // 5 minutes (fallback)
            };

            // Signal graceful recovery attempt instead of immediate factory reset
            let recovery_data = ResetNotificationData {
                device_id: self.device_id.clone(),
                reset_timestamp: get_iso8601_timestamp(),
                old_cert_arn: self.certificate_arn.clone(),
                reason: format!("graceful_recovery_attempt_{}", self.recovery_attempts),
            };

            info!(
                "🔄 Attempting graceful recovery for {}s before escalation",
                base_delay_secs
            );

            // TODO: Implement graceful recovery actions here:
            // - Retry WiFi connection
            // - Retry MQTT connection
            // - Reload certificates
            // - Check system health

            // For now, signal recovery attempt and wait
            RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetTriggered {
                reset_data: recovery_data,
            });

            // Wait for recovery period before allowing next attempt
            Timer::after(Duration::from_secs(base_delay_secs)).await;

            // Clear reset in progress flag to allow future attempts
            RESET_IN_PROGRESS.store(false, Ordering::SeqCst);

            info!("✅ Graceful recovery attempt completed");
            return Ok(());
        } else {
            // Tier 2: Factory Reset (Last Resort)
            info!("🔥 Tier 2: All graceful recovery attempts exhausted, proceeding with factory reset");

            // Reset recovery counter for future button presses
            self.recovery_attempts = 0;
            self.last_recovery_attempt = None;

            // Create factory reset notification data
            let reset_data = ResetNotificationData {
                device_id: self.device_id.clone(),
                reset_timestamp: get_iso8601_timestamp(),
                old_cert_arn: self.certificate_arn.clone(),
                reason: "physical_button_factory_reset_after_recovery_failure".to_string(),
            };

            // Signal that factory reset is triggered
            RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetTriggered { reset_data });

            info!("📡 Factory reset trigger signal sent to reset handler");

            // Reset manager's job is done - reset_handler will take over
            // Clear reset in progress flag will be handled by reset_handler
            RESET_IN_PROGRESS.store(false, Ordering::SeqCst);

            Ok(())
        }
    }

    // Note: update_certificate_arn and is_storage_initialized removed - no longer needed
}
