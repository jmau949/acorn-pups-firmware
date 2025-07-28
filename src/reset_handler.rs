// Reset Handler Module
// Factory reset execution with Echo/Nest-style reset security
// Generates device instance IDs and performs local data erasure

// Import Embassy synchronization primitives for task coordination
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

// Import Embassy time utilities for delays
use embassy_time::{Duration, Timer};

// Import logging macros for debug output
use log::{debug, error, info, warn};

// Import anyhow for error handling following existing patterns
use anyhow::{anyhow, Result};

// Import ESP-IDF NVS for data erasure
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};

// Import our system components
use crate::{SystemEvent, SYSTEM_EVENT_SIGNAL};

// Reset handler configuration constants
const SELECTIVE_ERASURE_TIMEOUT_MS: u64 = 5000; // 5 seconds for NVS erasure

// Reset handler events for external coordination
#[derive(Debug, Clone)]
pub enum ResetHandlerEvent {
    ResetStarted,
    InstanceIdGenerated(String),
    DataErased,
    ResetCompleted,
    ResetError(String),
}

// Global reset handler event signal
pub static RESET_HANDLER_EVENT_SIGNAL: Signal<CriticalSectionRawMutex, ResetHandlerEvent> =
    Signal::new();

/// Reset handler - manages factory reset with device instance ID security
pub struct ResetHandler {
    device_id: String,
    nvs_partition: Option<EspDefaultNvsPartition>,
}

impl ResetHandler {
    /// Create new reset handler
    pub fn new(device_id: String) -> Self {
        info!("🔧 Creating reset handler for device: {}", device_id);

        Self {
            device_id,
            nvs_partition: None,
        }
    }

    /// Initialize NVS partition for data erasure and check for power-loss recovery
    pub fn initialize_nvs_partition(
        &mut self,
        nvs_partition: EspDefaultNvsPartition,
    ) -> Result<()> {
        info!("🔧 Initializing NVS partition for reset handler");
        self.nvs_partition = Some(nvs_partition);

        // Check for incomplete reset on boot and recover
        if let Err(e) = self.check_and_recover_from_power_loss() {
            error!(
                "❌ Failed to recover from potential power-loss during reset: {}",
                e
            );
            // Continue anyway - device should still be functional
        }

        info!("✅ NVS partition initialized for reset handler");
        Ok(())
    }

    /// Check for incomplete reset and recover from power-loss scenarios
    fn check_and_recover_from_power_loss(&self) -> Result<()> {
        info!("🔍 Checking for incomplete reset state after boot");

        let nvs_partition = match &self.nvs_partition {
            Some(partition) => partition,
            None => {
                debug!("📝 No NVS partition - skipping power-loss recovery check");
                return Ok(());
            }
        };

        // Check for "reset in progress" marker
        let nvs = match EspNvs::new(nvs_partition.clone(), "reset_state", false) {
            Ok(nvs) => nvs,
            Err(_) => {
                debug!("📝 No reset_state namespace - normal boot");
                return Ok(());
            }
        };

        // Check for reset-in-progress marker
        let mut marker_buf = [0u8; 32];
        match nvs.get_str("reset_pending", &mut marker_buf) {
            Ok(Some("true")) => {
                warn!("⚠️ Found incomplete reset - power lost during reset!");
                warn!("🔄 Attempting to complete interrupted factory reset");

                // Complete the interrupted reset
                self.complete_interrupted_reset()?;
            }
            Ok(Some(_)) | Ok(None) => {
                debug!("📝 No reset-in-progress marker found");
            }
            Err(e) => {
                debug!("📝 Could not check reset-in-progress marker: {:?}", e);
            }
        }

        Ok(())
    }

    /// Complete an interrupted factory reset after power loss
    fn complete_interrupted_reset(&self) -> Result<()> {
        info!("🔄 Completing interrupted factory reset");

        // Generate new instance ID and complete reset
        let new_instance_id = self.generate_device_instance_id();

        // Store the reset state properly this time - create sync version for recovery
        self.store_reset_state_sync(&new_instance_id, "power_loss_recovery")?;

        // Ensure main device credentials are wiped
        self.wipe_main_device_credentials()?;

        // Clear the in-progress marker
        self.clear_reset_in_progress_marker()?;

        info!("✅ Successfully completed interrupted factory reset");
        warn!("⚠️ Device will now enter BLE setup mode on next reboot");

        Ok(())
    }

    /// Clear the reset-in-progress marker
    fn clear_reset_in_progress_marker(&self) -> Result<()> {
        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        let mut nvs = EspNvs::new(nvs_partition.clone(), "reset_state", true)
            .map_err(|e| anyhow!("Failed to open reset_state namespace: {:?}", e))?;

        match nvs.remove("reset_pending") {
            Ok(_) => info!("✅ Cleared reset-in-progress marker"),
            Err(e) => warn!("⚠️ Failed to clear reset-in-progress marker: {:?}", e),
        }

        Ok(())
    }

    /// Execute factory reset with Echo/Nest-style security and power-loss resilience
    /// Generates new device instance ID and wipes device data
    pub async fn execute_factory_reset(&mut self, reason: String) -> Result<()> {
        info!("🔥 Executing factory reset with device instance ID security");
        info!("📋 Reset reason: {}", reason);

        RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetStarted);

        // Step 1: Mark reset as in progress for power-loss recovery
        self.set_reset_in_progress_marker()?;

        // Step 2: Generate new device instance ID for reset security
        let new_instance_id = self.generate_device_instance_id();
        info!("🆔 Generated new device instance ID: {}", new_instance_id);

        RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::InstanceIdGenerated(
            new_instance_id.clone(),
        ));

        // Step 3: Store reset state that survives the data wipe
        match self.store_reset_state(&new_instance_id, &reason).await {
            Ok(_) => {
                info!("✅ Reset state stored successfully");
            }
            Err(e) => {
                error!("❌ Failed to store reset state: {}", e);
                // Clear the in-progress marker since we failed
                let _ = self.clear_reset_in_progress_marker();
                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetError(e.to_string()));
                return Err(e);
            }
        }

        // Step 4: Perform selective NVS erasure (preserving reset state)
        match self.perform_factory_reset().await {
            Ok(_) => {
                info!("✅ Factory reset completed successfully");

                // Step 5: Clear the in-progress marker - reset is complete
                if let Err(e) = self.clear_reset_in_progress_marker() {
                    warn!("⚠️ Failed to clear reset-in-progress marker: {}", e);
                    // Continue anyway - reset was successful
                }

                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::DataErased);
            }
            Err(e) => {
                error!("❌ Factory reset failed: {}", e);
                // Leave the in-progress marker - power-loss recovery will handle this
                warn!("🚨 Reset-in-progress marker left for recovery");
                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetError(e.to_string()));
                return Err(e);
            }
        }

        info!("🎯 Factory reset completed - device ready for re-registration");
        Ok(())
    }

    /// Generate new device instance ID (UUID v4)
    fn generate_device_instance_id(&self) -> String {
        use uuid::Uuid;

        // Generate proper RFC-compliant UUID v4 (random)
        // This changes every factory reset to prove physical access
        // Uses proper cryptographic randomness instead of predictable timestamp
        Uuid::new_v4().to_string()
    }

    /// Synchronous version of store_reset_state for power-loss recovery
    fn store_reset_state_sync(&self, instance_id: &str, reason: &str) -> Result<()> {
        info!(
            "💾 Storing reset state synchronously for recovery: {}",
            instance_id
        );

        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        // Create reset state structure
        let reset_state = ResetState {
            device_instance_id: instance_id.to_string(),
            device_state: "factory_reset".to_string(),
            reset_timestamp: self.get_iso8601_timestamp(),
            reset_reason: reason.to_string(),
        };

        // Serialize to JSON for atomic storage
        let reset_state_json = serde_json::to_string(&reset_state)
            .map_err(|e| anyhow!("Failed to serialize reset state: {}", e))?;

        // Open reset_state namespace
        let mut nvs = EspNvs::new(nvs_partition.clone(), "reset_state", true)
            .map_err(|e| anyhow!("Failed to open reset_state namespace: {:?}", e))?;

        // Store as single atomic operation
        nvs.set_str("reset_json", &reset_state_json)
            .map_err(|e| anyhow!("Failed to store reset state: {:?}", e))?;

        info!("✅ Reset state stored synchronously");
        Ok(())
    }

    /// Wipe main device credentials to ensure clean slate
    fn wipe_main_device_credentials(&self) -> Result<()> {
        info!("🧹 Wiping main device credentials");

        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        // List of namespaces to wipe for main device credentials
        let namespaces_to_wipe = [
            "acorn_device", // Main device namespace
            "wifi_config",  // WiFi credentials (CORRECT namespace name)
            "mqtt_certs",   // MQTT certificates
        ];

        for namespace in &namespaces_to_wipe {
            match EspNvs::new(nvs_partition.clone(), namespace, true) {
                Ok(mut nvs) => {
                    // Get all keys in this namespace
                    let keys_to_remove = match *namespace {
                        "acorn_device" => vec!["device_id", "serial_number", "firmware_version"],
                        "wifi_config" => vec![
                            "ssid",
                            "password",
                            "auth_token",
                            "device_name",
                            "user_timezone",
                            "timestamp",
                        ],
                        "mqtt_certs" => vec![
                            "device_cert",
                            "private_key",
                            "ca_cert",
                            "iot_endpoint",
                            "device_id",
                        ],
                        _ => vec![],
                    };

                    for key in &keys_to_remove {
                        match nvs.remove(key) {
                            Ok(_) => debug!("🗑️ Removed key: {}/{}", namespace, key),
                            Err(_) => debug!("📝 Key not found: {}/{}", namespace, key),
                        }
                    }

                    info!("✅ Wiped namespace: {}", namespace);
                }
                Err(_) => {
                    debug!("📝 Namespace not found: {}", namespace);
                }
            }
        }

        info!("✅ Main device credentials wiped");
        Ok(())
    }

    /// Set the reset-in-progress marker for power-loss recovery
    fn set_reset_in_progress_marker(&self) -> Result<()> {
        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        let mut nvs = EspNvs::new(nvs_partition.clone(), "reset_state", true)
            .map_err(|e| anyhow!("Failed to open reset_state namespace: {:?}", e))?;

        nvs.set_str("reset_pending", "true")
            .map_err(|e| anyhow!("Failed to set reset-in-progress marker: {:?}", e))?;

        info!("🚨 Set reset-in-progress marker for power-loss recovery");
        Ok(())
    }

    /// Store reset state as single atomic JSON blob to prevent partial writes and NVS wear
    async fn store_reset_state(&self, instance_id: &str, reason: &str) -> Result<()> {
        info!(
            "💾 Storing reset state atomically for instance ID: {}",
            instance_id
        );

        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        // Create reset state structure
        let reset_state = ResetState {
            device_instance_id: instance_id.to_string(),
            device_state: "factory_reset".to_string(),
            reset_timestamp: self.get_iso8601_timestamp(),
            reset_reason: reason.to_string(),
        };

        // Serialize to JSON for atomic storage (prevents partial writes)
        let reset_state_json = serde_json::to_string(&reset_state)
            .map_err(|e| anyhow!("Failed to serialize reset state: {}", e))?;

        // Open reset_state namespace (survives main data wipe)
        let mut nvs = EspNvs::new(nvs_partition.clone(), "reset_state", true)
            .map_err(|e| anyhow!("Failed to open reset_state namespace: {:?}", e))?;

        // Store as single atomic operation to prevent corruption
        match nvs.set_str("reset_json", &reset_state_json) {
            Ok(()) => {
                info!("✅ Reset state stored atomically");
                debug!("📝 Instance ID: {}", reset_state.device_instance_id);
                debug!("📝 State: {}", reset_state.device_state);
                debug!("📝 Timestamp: {}", reset_state.reset_timestamp);
                debug!("📝 Reason: {}", reset_state.reset_reason);
                Ok(())
            }
            Err(e) => {
                error!("❌ Failed to store reset state: {:?}", e);
                error!("💥 This could leave device in inconsistent state!");

                // TODO: Consider implementing rollback or alternative storage
                // For now, we'll error out to prevent silent failures
                Err(anyhow!("Critical: Reset state storage failed: {:?}", e))
            }
        }
    }

    /// Get current timestamp in proper ISO 8601 format using chrono
    fn get_iso8601_timestamp(&self) -> String {
        use chrono::Utc;

        // Use proper RFC3339/ISO8601 timestamp generation
        // This handles timezones, leap years, calendar correctness, etc.
        Utc::now().to_rfc3339()
    }

    /// Perform factory reset with selective NVS erasure
    async fn perform_factory_reset(&self) -> Result<()> {
        info!("🔥 Performing factory reset with selective NVS erasure");

        // Signal system that reset is in progress
        SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemError(
            "Factory reset in progress - selective NVS erasure".to_string(),
        ));

        // Wait a moment for the signal to be processed
        Timer::after(Duration::from_millis(500)).await;

        // Perform selective NVS erasure (preserving reset_state namespace)
        match self.perform_selective_erasure().await {
            Ok(_) => {
                info!("✅ Selective NVS erasure completed successfully");
                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::DataErased);
            }
            Err(e) => {
                error!("❌ Selective NVS erasure failed: {}", e);
                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetError(e.to_string()));
                // Continue with reboot even if erasure fails partially
            }
        }

        // Signal reset completion
        RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetCompleted);

        // Production system reboot
        info!("🔄 Factory reset completed - rebooting system");
        unsafe {
            esp_idf_svc::sys::esp_restart();
        }

        // This line will never be reached due to the restart above, but kept for function signature
        #[allow(unreachable_code)]
        Ok(())
    }

    /// Perform selective NVS erasure (preserve reset_state namespace)
    async fn perform_selective_erasure(&self) -> Result<()> {
        info!("🗑️ Performing selective NVS namespace erasure");

        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        // List of namespaces to erase (main device data)
        let namespaces_to_erase = [
            "acorn_device", // Main device configuration
            "wifi_config",  // WiFi credentials (CORRECT namespace name)
            "mqtt_certs",   // MQTT certificates
        ];

        let mut erasure_errors = Vec::new();

        for namespace in &namespaces_to_erase {
            info!("🗑️ Erasing NVS namespace: {}", namespace);

            match EspNvs::new(nvs_partition.clone(), namespace, true) {
                Ok(mut nvs) => {
                    // Erase all keys by removing known keys
                    let keys_to_remove = match *namespace {
                        "acorn_device" => vec!["device_id", "serial_number", "firmware_version"],
                        "wifi_config" => vec![
                            "ssid",
                            "password",
                            "auth_token",
                            "device_name",
                            "user_timezone",
                            "timestamp",
                        ],
                        "mqtt_certs" => vec![
                            "device_cert",
                            "private_key",
                            "ca_cert",
                            "iot_endpoint",
                            "device_id",
                        ],
                        _ => vec![],
                    };

                    let mut removed_count = 0;
                    for key in keys_to_remove {
                        if nvs.remove(key).unwrap_or(false) {
                            removed_count += 1;
                        }
                    }
                    info!(
                        "✅ Erased {} keys from namespace: {}",
                        removed_count, namespace
                    );
                }
                Err(e) => {
                    let error_msg = format!("Failed to open {}: {:?}", namespace, e);
                    warn!("⚠️ {}", error_msg);
                    erasure_errors.push(error_msg);
                }
            }
        }

        if !erasure_errors.is_empty() {
            warn!("⚠️ Some namespaces failed to erase completely:");
            for error in &erasure_errors {
                warn!("  - {}", error);
            }
            // Don't fail completely - partial erasure is acceptable for reset
        }

        info!("✅ Selective NVS erasure completed");
        info!("🔒 Reset state namespace preserved for registration security");

        Ok(())
    }

    /// Load reset state from single JSON blob with enhanced validation
    pub fn load_reset_state(&self) -> Result<Option<ResetState>> {
        info!("📖 Loading reset state atomically from NVS");

        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        // Try to open reset_state namespace
        let nvs = match EspNvs::new(nvs_partition.clone(), "reset_state", false) {
            Ok(nvs) => nvs,
            Err(_) => {
                info!("📝 No reset state found - normal operation");
                return Ok(None);
            }
        };

        // Conservative buffer size with safety margin
        const MAX_RESET_JSON_SIZE: usize = 512;
        let mut json_buf = vec![0u8; MAX_RESET_JSON_SIZE];

        let reset_state_json = match nvs.get_str("reset_json", &mut json_buf) {
            Ok(Some(json_str)) => {
                // Validate JSON size before parsing
                if json_str.len() > MAX_RESET_JSON_SIZE - 100 {
                    warn!("Reset state JSON too large: {} bytes", json_str.len());
                    error!("🗑️ Oversized reset state - clearing for safety");
                    let _ = self.clear_reset_state();
                    return Ok(None);
                }
                json_str
            }
            Ok(None) => {
                info!("📝 No reset state JSON found - normal operation");
                return Ok(None);
            }
            Err(e) => {
                warn!("⚠️ Failed to load reset state JSON: {:?}", e);
                return Ok(None);
            }
        };

        // Deserialize from JSON with comprehensive error handling
        match serde_json::from_str::<ResetState>(reset_state_json) {
            Ok(reset_state) => {
                // Validate field sizes to prevent corruption attacks
                if reset_state.device_instance_id.len() > 64 {
                    warn!(
                        "Device instance ID too large: {} chars",
                        reset_state.device_instance_id.len()
                    );
                    let _ = self.clear_reset_state();
                    return Ok(None);
                }

                if reset_state.device_state.len() > 32 {
                    warn!(
                        "Device state too large: {} chars",
                        reset_state.device_state.len()
                    );
                    let _ = self.clear_reset_state();
                    return Ok(None);
                }

                if reset_state.reset_timestamp.len() > 64 {
                    warn!(
                        "Reset timestamp too large: {} chars",
                        reset_state.reset_timestamp.len()
                    );
                    let _ = self.clear_reset_state();
                    return Ok(None);
                }

                if reset_state.reset_reason.len() > 128 {
                    warn!(
                        "Reset reason too large: {} chars",
                        reset_state.reset_reason.len()
                    );
                    let _ = self.clear_reset_state();
                    return Ok(None);
                }

                info!("✅ Reset state loaded and validated successfully");
                debug!("📝 Instance ID: {}", reset_state.device_instance_id);
                debug!("📝 State: {}", reset_state.device_state);
                debug!("📝 Timestamp: {}", reset_state.reset_timestamp);
                debug!("📝 Reason: {}", reset_state.reset_reason);

                Ok(Some(reset_state))
            }
            Err(e) => {
                warn!("Failed to parse reset state JSON: {}", e);
                warn!("🗑️ Corrupted reset state - clearing for safety");

                // Clear corrupted data to prevent repeated failures
                if let Err(clear_err) = self.clear_reset_state() {
                    error!("❌ Failed to clear corrupted reset state: {}", clear_err);
                }

                Ok(None)
            }
        }
    }

    /// Clear reset state after successful registration
    pub fn clear_reset_state(&self) -> Result<()> {
        info!("🗑️ Clearing reset state after successful registration");

        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        let mut nvs = EspNvs::new(nvs_partition.clone(), "reset_state", true)
            .map_err(|e| anyhow!("Failed to open reset_state namespace: {:?}", e))?;

        // Remove the JSON blob completely
        match nvs.remove("reset_json") {
            Ok(_) => {
                info!("✅ Reset state cleared completely - device now in normal operation");
                Ok(())
            }
            Err(e) => {
                warn!("⚠️ Failed to clear reset state (may not exist): {:?}", e);
                // Don't error out - clearing non-existent state is OK
                Ok(())
            }
        }
    }
}

/// Reset state structure for registration API and JSON serialization
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResetState {
    pub device_instance_id: String,
    pub device_state: String,
    pub reset_timestamp: String,
    pub reset_reason: String,
}
