// Reset Storage Module
// NVS storage management for reset state persistence and selective namespace erasure
// Handles reset notification storage, selective NVS cleanup, and data integrity

// Import ESP-IDF's NVS (Non-Volatile Storage) functionality
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
// ESP error types handled by esp_idf_svc

// Import logging macros for debug output
use log::{debug, info, warn};

// Import Serde traits for JSON serialization
use serde::{Deserialize, Serialize};

// Import anyhow for error handling following existing patterns
use anyhow::{anyhow, Result};

// Import time utilities for proper ISO 8601 timestamps
use chrono::{DateTime, Utc};

// NVS namespace constants
const RESET_PENDING_NAMESPACE: &str = "reset_pending";
const ACORN_DEVICE_NAMESPACE: &str = "acorn_device";
const WIFI_CONFIG_NAMESPACE: &str = "wifi_config";
const MQTT_CERTS_NAMESPACE: &str = "mqtt_certs";

// NVS storage keys for reset state
const PENDING_FLAG_KEY: &str = "pending";
const DEVICE_ID_KEY: &str = "device_id";
const RESET_TIMESTAMP_KEY: &str = "reset_timestamp";
const OLD_CERT_ARN_KEY: &str = "old_cert_arn";
const RESET_REASON_KEY: &str = "reason";

/// Reset notification data structure for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetNotificationData {
    pub device_id: String,
    pub reset_timestamp: String, // ISO 8601 format
    pub old_cert_arn: String,
    pub reason: String,
}

/// Reset storage manager - handles all NVS operations for reset functionality
pub struct ResetStorage {
    nvs_reset: EspNvs<NvsDefault>, // NVS handle for reset_pending namespace
}

impl ResetStorage {
    /// Create reset storage using provided NVS partition
    pub fn new_with_partition(nvs_partition: EspDefaultNvsPartition) -> Result<Self> {
        info!("🔄 Initializing reset storage with provided NVS partition");

        // Open the NVS namespace for reset state storage
        let nvs_reset = EspNvs::new(nvs_partition, RESET_PENDING_NAMESPACE, true)
            .map_err(|e| anyhow!("Failed to open reset_pending NVS namespace: {}", e))?;

        info!("✅ Reset storage initialized successfully");
        Ok(Self { nvs_reset })
    }

    /// Check if a reset notification is pending
    pub fn is_reset_pending(&mut self) -> Result<bool> {
        match self.nvs_reset.get_u8(PENDING_FLAG_KEY) {
            Ok(Some(value)) => {
                let pending = value == 1;
                debug!("🔍 Reset pending check: {}", pending);
                Ok(pending)
            }
            Ok(None) => {
                debug!("🔍 No reset pending flag found");
                Ok(false)
            }
            Err(e) => {
                warn!("⚠️ Failed to read reset pending flag: {}", e);
                Ok(false) // Default to false on error
            }
        }
    }

    /// Store reset notification data for deferred processing
    pub fn store_reset_notification(&mut self, data: &ResetNotificationData) -> Result<()> {
        info!(
            "💾 Storing reset notification data for device: {}",
            data.device_id
        );

        // Store pending flag
        self.nvs_reset
            .set_u8(PENDING_FLAG_KEY, 1)
            .map_err(|e| anyhow!("Failed to set pending flag: {}", e))?;

        // Store device ID
        self.nvs_reset
            .set_str(DEVICE_ID_KEY, &data.device_id)
            .map_err(|e| anyhow!("Failed to store device ID: {}", e))?;

        // Store reset timestamp
        self.nvs_reset
            .set_str(RESET_TIMESTAMP_KEY, &data.reset_timestamp)
            .map_err(|e| anyhow!("Failed to store reset timestamp: {}", e))?;

        // Store old certificate ARN
        self.nvs_reset
            .set_str(OLD_CERT_ARN_KEY, &data.old_cert_arn)
            .map_err(|e| anyhow!("Failed to store old certificate ARN: {}", e))?;

        // Store reset reason
        self.nvs_reset
            .set_str(RESET_REASON_KEY, &data.reason)
            .map_err(|e| anyhow!("Failed to store reset reason: {}", e))?;

        // NVS changes are automatically committed in esp-idf-svc

        info!("✅ Reset notification data stored successfully");
        Ok(())
    }

    /// Load stored reset notification data
    pub fn load_reset_notification(&mut self) -> Result<Option<ResetNotificationData>> {
        if !self.is_reset_pending()? {
            return Ok(None);
        }

        info!("📂 Loading stored reset notification data");

        // Load device ID
        let mut device_id_buf = [0u8; 128];
        let device_id = self
            .nvs_reset
            .get_str(DEVICE_ID_KEY, &mut device_id_buf)
            .map_err(|e| anyhow!("Failed to load device ID: {}", e))?
            .ok_or_else(|| anyhow!("Device ID not found in reset storage"))?;

        // Load reset timestamp
        let mut timestamp_buf = [0u8; 64];
        let reset_timestamp = self
            .nvs_reset
            .get_str(RESET_TIMESTAMP_KEY, &mut timestamp_buf)
            .map_err(|e| anyhow!("Failed to load reset timestamp: {}", e))?
            .ok_or_else(|| anyhow!("Reset timestamp not found in reset storage"))?;

        // Load old certificate ARN
        let mut cert_arn_buf = [0u8; 256];
        let old_cert_arn = self
            .nvs_reset
            .get_str(OLD_CERT_ARN_KEY, &mut cert_arn_buf)
            .map_err(|e| anyhow!("Failed to load old certificate ARN: {}", e))?
            .ok_or_else(|| anyhow!("Old certificate ARN not found in reset storage"))?;

        // Load reset reason
        let mut reason_buf = [0u8; 64];
        let reason = self
            .nvs_reset
            .get_str(RESET_REASON_KEY, &mut reason_buf)
            .map_err(|e| anyhow!("Failed to load reset reason: {}", e))?
            .ok_or_else(|| anyhow!("Reset reason not found in reset storage"))?;

        let notification_data = ResetNotificationData {
            device_id: device_id.to_string(),
            reset_timestamp: reset_timestamp.to_string(),
            old_cert_arn: old_cert_arn.to_string(),
            reason: reason.to_string(),
        };

        info!("✅ Reset notification data loaded successfully");
        Ok(Some(notification_data))
    }

    /// Clear reset notification data after successful delivery
    pub fn clear_reset_notification(&mut self) -> Result<()> {
        info!("🧹 Clearing reset notification data");

        // Clear all reset notification keys
        if let Err(e) = self.nvs_reset.remove(PENDING_FLAG_KEY) {
            warn!("⚠️ Failed to remove pending flag: {}", e);
        }

        if let Err(e) = self.nvs_reset.remove(DEVICE_ID_KEY) {
            warn!("⚠️ Failed to remove device ID: {}", e);
        }

        if let Err(e) = self.nvs_reset.remove(RESET_TIMESTAMP_KEY) {
            warn!("⚠️ Failed to remove reset timestamp: {}", e);
        }

        if let Err(e) = self.nvs_reset.remove(OLD_CERT_ARN_KEY) {
            warn!("⚠️ Failed to remove old certificate ARN: {}", e);
        }

        if let Err(e) = self.nvs_reset.remove(RESET_REASON_KEY) {
            warn!("⚠️ Failed to remove reset reason: {}", e);
        }

        // NVS changes are automatically committed in esp-idf-svc

        info!("✅ Reset notification data cleared successfully");
        Ok(())
    }

    /// Perform selective NVS erasure for factory reset
    /// Clears device configuration while preserving reset notification data
    pub fn perform_selective_erasure(nvs_partition: EspDefaultNvsPartition) -> Result<()> {
        info!("🔥 Performing selective NVS erasure for factory reset");

        // List of namespaces to erase during factory reset
        let namespaces_to_erase = [
            ACORN_DEVICE_NAMESPACE,
            WIFI_CONFIG_NAMESPACE,
            MQTT_CERTS_NAMESPACE,
        ];

        for namespace in &namespaces_to_erase {
            match Self::erase_namespace(nvs_partition.clone(), namespace) {
                Ok(_) => {
                    info!("✅ Successfully erased namespace: {}", namespace);
                }
                Err(e) => {
                    warn!("⚠️ Failed to erase namespace {}: {}", namespace, e);
                    // Continue with other namespaces even if one fails
                }
            }
        }

        info!("🎯 Selective NVS erasure completed - reset_pending namespace preserved");
        Ok(())
    }

    /// Erase a specific NVS namespace by removing all known keys
    fn erase_namespace(nvs_partition: EspDefaultNvsPartition, namespace: &str) -> Result<()> {
        debug!("🗑️ Erasing NVS namespace: {}", namespace);

        let mut nvs = EspNvs::new(nvs_partition, namespace, true)
            .map_err(|e| anyhow!("Failed to open namespace {}: {}", namespace, e))?;

        // Define known keys for each namespace to erase
        let keys_to_remove = match namespace {
            WIFI_CONFIG_NAMESPACE => vec!["ssid", "password", "wifi_creds"],
            MQTT_CERTS_NAMESPACE => vec![
                "device_cert",
                "private_key",
                "root_ca",
                "cert_metadata",
                "device_id",
                "cert_arn",
                "endpoint",
                "created_at",
            ],
            ACORN_DEVICE_NAMESPACE => vec![
                "device_id",
                "firmware_version",
                "device_config",
                "auth_token",
                "registration_status",
                "last_heartbeat",
            ],
            _ => {
                warn!("Unknown namespace for erasure: {}", namespace);
                return Ok(());
            }
        };

        // Remove each key in the namespace
        let mut removed_count = 0;
        for key in keys_to_remove {
            match nvs.remove(key) {
                Ok(existed) => {
                    if existed {
                        debug!("🗑️ Removed key: {}", key);
                        removed_count += 1;
                    } else {
                        debug!("🔍 Key not found: {}", key);
                    }
                }
                Err(e) => {
                    // Error removing key, log and continue
                    debug!("⚠️ Error removing key {}: {}", key, e);
                }
            }
        }

        info!(
            "✅ Erased {} keys from namespace: {}",
            removed_count, namespace
        );
        Ok(())
    }

    /// Debug utility to dump reset storage contents
    pub fn debug_dump_storage(&mut self) {
        debug!("🔍 DEBUG: Reset storage contents:");

        match self.is_reset_pending() {
            Ok(pending) => debug!("  Pending: {}", pending),
            Err(e) => debug!("  Pending: Error reading - {}", e),
        }

        // Try to read each key for debugging
        let keys = [
            DEVICE_ID_KEY,
            RESET_TIMESTAMP_KEY,
            OLD_CERT_ARN_KEY,
            RESET_REASON_KEY,
        ];

        for key in &keys {
            match self.nvs_reset.get_str(key, &mut [0u8; 256]) {
                Ok(Some(value)) => debug!("  {}: {}", key, value),
                Ok(None) => debug!("  {}: None", key),
                Err(e) => debug!("  {}: Error - {}", key, e),
            }
        }
    }

    /// Validate reset notification data integrity
    pub fn validate_reset_data(&mut self) -> Result<bool> {
        debug!("🔍 Validating reset notification data integrity");

        if !self.is_reset_pending()? {
            return Ok(true); // No data to validate
        }

        // Check that all required fields are present
        let required_keys = [
            DEVICE_ID_KEY,
            RESET_TIMESTAMP_KEY,
            OLD_CERT_ARN_KEY,
            RESET_REASON_KEY,
        ];

        for key in &required_keys {
            match self.nvs_reset.get_str(key, &mut [0u8; 256]) {
                Ok(Some(_)) => {} // Key exists, continue
                Ok(None) => {
                    warn!("⚠️ Reset data validation failed: missing key {}", key);
                    return Ok(false);
                }
                Err(e) => {
                    warn!("⚠️ Reset data validation error reading {}: {}", key, e);
                    return Ok(false);
                }
            }
        }

        debug!("✅ Reset notification data validation passed");
        Ok(true)
    }

    /// Generate current timestamp in ISO 8601 format
    pub fn generate_reset_timestamp() -> String {
        let now: DateTime<Utc> = Utc::now();
        now.to_rfc3339()
    }
}
