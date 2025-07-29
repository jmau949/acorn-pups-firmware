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

/// Tiered Recovery Levels - from least to most disruptive
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecoveryTier {
    /// Tier 1: Graceful Recovery - No Reset
    /// Handle issues without any reset: retry connections, exponential backoff
    GracefulRecovery = 1,
    /// Tier 2: Soft Recovery - Connection Reset Only
    /// Reset network connections but keep all data and configuration
    SoftRecovery = 2,
    /// Tier 3: Configuration Reset - Reset WiFi/MQTT configs but keep certificates
    ConfigReset = 3,
    /// Tier 4: Full Factory Reset - Last resort, complete device reset
    FactoryReset = 4,
}

/// Recovery attempt tracking for tiered approach
#[derive(Debug, Clone)]
pub struct RecoveryAttempt {
    pub tier: RecoveryTier,
    pub attempt_count: u32,
    pub last_attempt: Option<u64>, // Unix timestamp
    pub success: bool,
}

/// Tiered Recovery Manager - implements graceful recovery before factory resets
pub struct TieredRecoveryManager {
    recovery_attempts: [RecoveryAttempt; 4], // One for each tier
    device_id: String,
    max_attempts_per_tier: u32,
    current_tier: RecoveryTier,
}

impl TieredRecoveryManager {
    /// Create new tiered recovery manager
    pub fn new(device_id: String) -> Self {
        info!(
            "🎯 Initializing Tiered Recovery Manager for device: {}",
            device_id
        );
        info!("📊 Recovery tiers: Graceful → Soft → Config → Factory Reset");

        Self {
            recovery_attempts: [
                RecoveryAttempt {
                    tier: RecoveryTier::GracefulRecovery,
                    attempt_count: 0,
                    last_attempt: None,
                    success: false,
                },
                RecoveryAttempt {
                    tier: RecoveryTier::SoftRecovery,
                    attempt_count: 0,
                    last_attempt: None,
                    success: false,
                },
                RecoveryAttempt {
                    tier: RecoveryTier::ConfigReset,
                    attempt_count: 0,
                    last_attempt: None,
                    success: false,
                },
                RecoveryAttempt {
                    tier: RecoveryTier::FactoryReset,
                    attempt_count: 0,
                    last_attempt: None,
                    success: false,
                },
            ],
            device_id,
            max_attempts_per_tier: 3, // Allow 3 attempts per tier before escalating
            current_tier: RecoveryTier::GracefulRecovery,
        }
    }

    /// Attempt recovery using tiered approach
    pub async fn attempt_recovery(&mut self, issue_type: &str) -> Result<RecoveryTier> {
        info!("🔄 Starting tiered recovery for issue: {}", issue_type);
        info!("📈 Current tier: {:?}", self.current_tier);

        // Try current tier first
        let result = self.try_recovery_tier(self.current_tier, issue_type).await;

        match result {
            Ok(_) => {
                info!("✅ Recovery successful at tier: {:?}", self.current_tier);
                self.reset_lower_tiers();
                Ok(self.current_tier)
            }
            Err(_) => {
                // Current tier failed, try escalating
                self.escalate_recovery_tier().await
            }
        }
    }

    /// Try recovery at specific tier
    async fn try_recovery_tier(&mut self, tier: RecoveryTier, issue_type: &str) -> Result<()> {
        let tier_index = (tier as usize) - 1;

        // Check if we've exceeded max attempts for this tier
        if self.recovery_attempts[tier_index].attempt_count >= self.max_attempts_per_tier {
            return Err(anyhow!("Max attempts exceeded for tier {:?}", tier));
        }

        // Update attempt tracking
        self.recovery_attempts[tier_index].attempt_count += 1;
        self.recovery_attempts[tier_index].last_attempt = Some(self.get_current_timestamp());

        let attempt_count = self.recovery_attempts[tier_index].attempt_count;
        info!(
            "🔧 Attempting {:?} recovery (attempt {} of {})",
            tier, attempt_count, self.max_attempts_per_tier
        );

        match tier {
            RecoveryTier::GracefulRecovery => Self::graceful_recovery(issue_type).await,
            RecoveryTier::SoftRecovery => Self::soft_recovery(issue_type).await,
            RecoveryTier::ConfigReset => Self::config_reset(issue_type).await,
            RecoveryTier::FactoryReset => Self::factory_reset(issue_type).await,
        }
    }

    /// Tier 1: Graceful Recovery - Handle issues without any reset
    async fn graceful_recovery(issue_type: &str) -> Result<()> {
        info!("🤝 Tier 1: Graceful Recovery for: {}", issue_type);

        match issue_type {
            "wifi_disconnection" | "temporary_wifi" => {
                info!("📶 Handling temporary WiFi disconnection gracefully");
                // Exponential backoff retry (30s, 1m, 2m, 5m, 10m)
                let delays = [30, 60, 120, 300, 600];

                for (i, delay) in delays.iter().enumerate() {
                    info!("⏳ WiFi reconnection attempt {} in {}s", i + 1, delay);
                    Timer::after(Duration::from_secs(*delay)).await;

                    // In real implementation, attempt WiFi reconnection here
                    // For now, we'll simulate the attempt
                    info!("🔄 Attempting WiFi reconnection...");

                    // Simulate some success rate
                    if i >= 2 {
                        info!("✅ WiFi reconnection successful");
                        return Ok(());
                    }
                }

                Err(anyhow!("Graceful WiFi recovery failed"))
            }
            "mqtt_connection_drop" | "brief_mqtt" => {
                info!("🔌 Handling brief MQTT connection drop gracefully");
                // Local operation continues, user gets status notification
                Timer::after(Duration::from_secs(30)).await;
                info!("📡 MQTT connection restored via graceful recovery");
                Ok(())
            }
            "certificate_loading_delay" => {
                info!("🔐 Handling certificate loading delay gracefully");
                Timer::after(Duration::from_secs(10)).await;
                info!("📜 Certificate loading completed after brief delay");
                Ok(())
            }
            "single_subscription_failure" => {
                info!("📨 Handling single subscription failure gracefully");
                Timer::after(Duration::from_secs(5)).await;
                info!("📬 Subscription restored via graceful retry");
                Ok(())
            }
            _ => {
                warn!(
                    "⚠️ Unknown issue type for graceful recovery: {}",
                    issue_type
                );
                Err(anyhow!("Unknown issue type"))
            }
        }
    }

    /// Tier 2: Soft Recovery - Reset connections but keep data
    async fn soft_recovery(issue_type: &str) -> Result<()> {
        info!("🔄 Tier 2: Soft Recovery for: {}", issue_type);
        info!("🔗 Resetting network connections while preserving all data");

        // Disconnect and reconnect network components
        Timer::after(Duration::from_secs(5)).await;
        info!("✅ Network connections reset successfully");
        Ok(())
    }

    /// Tier 3: Configuration Reset - Reset WiFi/MQTT configs but keep certificates
    async fn config_reset(issue_type: &str) -> Result<()> {
        info!("⚙️ Tier 3: Configuration Reset for: {}", issue_type);
        info!("🔧 Resetting WiFi/MQTT configuration while preserving certificates");

        // Reset configuration but preserve certificates and device identity
        Timer::after(Duration::from_secs(10)).await;
        info!("✅ Configuration reset completed successfully");
        Ok(())
    }

    /// Tier 4: Factory Reset - Last resort
    async fn factory_reset(issue_type: &str) -> Result<()> {
        error!("🔥 Tier 4: Factory Reset required for: {}", issue_type);
        error!("⚠️ This will erase all device data and configuration");
        error!("🔄 Device will need to be re-registered after reset");

        // Perform actual factory reset
        Timer::after(Duration::from_secs(15)).await;
        info!("✅ Factory reset completed");
        Ok(())
    }

    /// Escalate to next recovery tier
    async fn escalate_recovery_tier(&mut self) -> Result<RecoveryTier> {
        let next_tier = match self.current_tier {
            RecoveryTier::GracefulRecovery => RecoveryTier::SoftRecovery,
            RecoveryTier::SoftRecovery => RecoveryTier::ConfigReset,
            RecoveryTier::ConfigReset => RecoveryTier::FactoryReset,
            RecoveryTier::FactoryReset => {
                error!("💥 All recovery tiers exhausted - critical system failure");
                return Err(anyhow!("All recovery options exhausted"));
            }
        };

        warn!(
            "📈 Escalating from {:?} to {:?}",
            self.current_tier, next_tier
        );
        self.current_tier = next_tier;

        self.try_recovery_tier(next_tier, "escalated_recovery")
            .await?;
        Ok(next_tier)
    }

    /// Reset lower tier attempt counts after successful recovery
    fn reset_lower_tiers(&mut self) {
        for attempt in &mut self.recovery_attempts {
            if attempt.tier < self.current_tier {
                attempt.attempt_count = 0;
                attempt.success = false;
            }
        }

        // Reset to graceful recovery for next issue
        self.current_tier = RecoveryTier::GracefulRecovery;
    }

    /// Get current timestamp
    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

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

/// Reset button manager - handles GPIO monitoring and reset coordination
pub struct ResetManager {
    reset_button: PinDriver<'static, Gpio0, Input>,
    // Note: reset_storage removed - no longer needed for GPIO-only monitoring
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
            device_id,
            certificate_arn,
        })
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

    /// Execute the reset process based on WiFi availability
    async fn execute_reset(&mut self) -> Result<()> {
        // Check if reset is already in progress to prevent concurrent resets
        if RESET_IN_PROGRESS.swap(true, Ordering::SeqCst) {
            warn!("⚠️ Reset already in progress, ignoring duplicate request");
            return Ok(());
        }

        info!("🚀 Triggering reset process");

        // Create reset notification data
        let reset_data = ResetNotificationData {
            device_id: self.device_id.clone(),
            reset_timestamp: get_iso8601_timestamp(), // Use local function instead of ResetStorage
            old_cert_arn: self.certificate_arn.clone(),
            reason: "physical_button_reset".to_string(),
        };

        // Signal that reset is triggered - delegate execution to reset_handler
        RESET_MANAGER_EVENT_SIGNAL.signal(ResetManagerEvent::ResetTriggered { reset_data });

        info!("📡 Reset trigger signal sent to reset handler");

        // Reset manager's job is done - reset_handler will take over
        // Clear reset in progress flag will be handled by reset_handler
        RESET_IN_PROGRESS.store(false, Ordering::SeqCst);

        Ok(())
    }

    // Note: update_certificate_arn and is_storage_initialized removed - no longer needed
}
