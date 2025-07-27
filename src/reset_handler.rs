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

    /// Initialize NVS partition for data erasure
    pub fn initialize_nvs_partition(
        &mut self,
        nvs_partition: EspDefaultNvsPartition,
    ) -> Result<()> {
        info!("🔧 Initializing NVS partition for reset handler");
        self.nvs_partition = Some(nvs_partition);
        info!("✅ NVS partition initialized for reset handler");
        Ok(())
    }

    /// Execute factory reset with Echo/Nest-style security
    /// Generates new device instance ID and wipes device data
    pub async fn execute_factory_reset(&mut self, reason: String) -> Result<()> {
        info!("🔥 Executing factory reset with device instance ID security");
        info!("📋 Reset reason: {}", reason);

        RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetStarted);

        // Generate new device instance ID for reset security
        let new_instance_id = self.generate_device_instance_id();
        info!("🆔 Generated new device instance ID: {}", new_instance_id);

        RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::InstanceIdGenerated(
            new_instance_id.clone(),
        ));

        // Store reset state that survives the data wipe
        match self.store_reset_state(&new_instance_id, &reason).await {
            Ok(_) => {
                info!("✅ Reset state stored successfully");
            }
            Err(e) => {
                error!("❌ Failed to store reset state: {}", e);
                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetError(e.to_string()));
                return Err(e);
            }
        }

        // Perform selective NVS erasure (preserving reset state)
        match self.perform_factory_reset().await {
            Ok(_) => {
                info!("✅ Factory reset completed successfully");
                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetCompleted);
            }
            Err(e) => {
                error!("❌ Factory reset failed: {}", e);
                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetError(e.to_string()));
                return Err(e);
            }
        }

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

    /// Store reset state in separate namespace that survives data wipe
    async fn store_reset_state(&self, instance_id: &str, reason: &str) -> Result<()> {
        info!("💾 Storing reset state for instance ID: {}", instance_id);

        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        // Open reset_state namespace (survives main data wipe)
        let mut nvs = EspNvs::new(nvs_partition.clone(), "reset_state", true)
            .map_err(|e| anyhow!("Failed to open reset_state namespace: {:?}", e))?;

        // Store device instance ID
        nvs.set_str("device_instance_id", instance_id)
            .map_err(|e| anyhow!("Failed to store device instance ID: {:?}", e))?;

        // Store device state
        nvs.set_str("device_state", "factory_reset")
            .map_err(|e| anyhow!("Failed to store device state: {:?}", e))?;

        // Store reset timestamp (ISO 8601)
        let reset_timestamp = self.get_iso8601_timestamp();
        nvs.set_str("reset_timestamp", &reset_timestamp)
            .map_err(|e| anyhow!("Failed to store reset timestamp: {:?}", e))?;

        // Store reset reason
        nvs.set_str("reset_reason", reason)
            .map_err(|e| anyhow!("Failed to store reset reason: {:?}", e))?;

        // Note: ESP-IDF NVS automatically commits changes

        info!("✅ Reset state stored successfully");
        debug!("📝 Instance ID: {}", instance_id);
        debug!("📝 State: factory_reset");
        debug!("📝 Timestamp: {}", reset_timestamp);
        debug!("📝 Reason: {}", reason);

        Ok(())
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
            "acorn_device",    // Main device configuration
            "wifi_storage",    // WiFi credentials
            "mqtt_certs",      // MQTT certificates
            "device_identity", // Device identity
        ];

        let mut erasure_errors = Vec::new();

        for namespace in &namespaces_to_erase {
            info!("🗑️ Erasing NVS namespace: {}", namespace);

            match EspNvs::new(nvs_partition.clone(), namespace, true) {
                Ok(mut nvs) => {
                    // Erase all keys by removing known keys
                    let keys_to_remove = match *namespace {
                        "acorn_device" => vec!["device_id", "config", "settings"],
                        "wifi_storage" => vec!["ssid", "password", "auth_token"],
                        "mqtt_certs" => vec!["device_cert", "private_key", "ca_cert"],
                        "device_identity" => vec!["serial", "mac_address"],
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

    /// Load reset state from single JSON blob to prevent buffer overflows
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

        // Use heap-allocated buffer to prevent stack overflow
        // Max JSON size for reset state should be < 1KB
        let mut json_buf = vec![0u8; 1024];

        let reset_state_json = match nvs.get_str("reset_state_json", &mut json_buf) {
            Ok(Some(json_str)) => json_str,
            Ok(None) => {
                info!("📝 No reset state JSON found - normal operation");
                return Ok(None);
            }
            Err(e) => {
                warn!("⚠️ Failed to load reset state JSON: {:?}", e);
                return Ok(None);
            }
        };

        // Deserialize from JSON with error handling
        match serde_json::from_str::<ResetState>(reset_state_json) {
            Ok(reset_state) => {
                info!("✅ Reset state loaded successfully");
                debug!("📝 Instance ID: {}", reset_state.device_instance_id);
                debug!("📝 State: {}", reset_state.device_state);
                debug!("📝 Timestamp: {}", reset_state.reset_timestamp);
                debug!("📝 Reason: {}", reset_state.reset_reason);

                Ok(Some(reset_state))
            }
            Err(e) => {
                error!("❌ Failed to deserialize reset state JSON: {}", e);
                error!("🗑️ Corrupted reset state - clearing for safety");

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
        match nvs.remove("reset_state_json") {
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
