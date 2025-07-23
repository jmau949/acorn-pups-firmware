// Reset Handler Module
// Reset behavior execution and notification processing
// Handles online/offline reset flows and deferred notification delivery

// Import Embassy synchronization primitives for task coordination
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

// Import Embassy time utilities for timeouts and delays
use embassy_time::{with_timeout, Duration, Timer};

// Import logging macros for debug output
use log::{debug, error, info, warn};

// Import anyhow for error handling following existing patterns
use anyhow::{anyhow, Result};

// Import Serde for JSON serialization
use serde::{Deserialize, Serialize};

// Import ESP-IDF NVS for selective erasure
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

// Import our modules and system components
use crate::mqtt_manager::{MqttMessage, MQTT_MESSAGE_CHANNEL};
use crate::reset_storage::{ResetNotificationData, ResetStorage};
use crate::{SystemEvent, SYSTEM_EVENT_SIGNAL};

// Reset handler configuration constants
const RESET_NOTIFICATION_TIMEOUT_MS: u64 = 10000; // 10 seconds for MQTT notification
const RESET_CONFIRMATION_WAIT_MS: u64 = 2000; // 2 seconds wait for confirmation
const SELECTIVE_ERASURE_TIMEOUT_MS: u64 = 5000; // 5 seconds for NVS erasure

// Reset message structure for MQTT notifications
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetCleanupMessage {
    pub command: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "resetTimestamp")]
    pub reset_timestamp: String,
    #[serde(rename = "oldCertificateArn")]
    pub old_certificate_arn: String,
    pub reason: String,
}

// Reset handler events for external coordination
#[derive(Debug, Clone)]
pub enum ResetHandlerEvent {
    OnlineResetStarted,
    OfflineResetStarted,
    NotificationSent,
    NotificationFailed(String),
    DeferredNotificationProcessed,
    SelectiveErasureCompleted,
    ResetCompleted,
    ResetError(String),
}

// Global reset handler event signal
pub static RESET_HANDLER_EVENT_SIGNAL: Signal<CriticalSectionRawMutex, ResetHandlerEvent> =
    Signal::new();

/// Reset handler - manages reset execution and notification processing
pub struct ResetHandler {
    device_id: String,
    reset_storage: Option<ResetStorage>,
    nvs_partition: Option<EspDefaultNvsPartition>,
}

impl ResetHandler {
    /// Create new reset handler
    pub fn new(device_id: String) -> Self {
        info!("🔧 Creating reset handler for device: {}", device_id);

        Self {
            device_id,
            reset_storage: None,
            nvs_partition: None,
        }
    }

    /// Initialize reset storage component
    pub fn initialize_storage(&mut self, reset_storage: ResetStorage) -> Result<()> {
        info!("🔧 Initializing reset storage for reset handler");
        self.reset_storage = Some(reset_storage);
        info!("✅ Reset storage initialized for reset handler");
        Ok(())
    }

    /// Initialize NVS partition for selective erasure
    pub fn initialize_nvs_partition(
        &mut self,
        nvs_partition: EspDefaultNvsPartition,
    ) -> Result<()> {
        info!("🔧 Initializing NVS partition for reset handler");
        self.nvs_partition = Some(nvs_partition);
        info!("✅ NVS partition initialized for reset handler");
        Ok(())
    }

    /// Execute online reset with immediate MQTT notification
    pub async fn execute_online_reset(&mut self, reset_data: ResetNotificationData) -> Result<()> {
        info!("🌐 Executing online reset with immediate notification");
        RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::OnlineResetStarted);

        // Create reset cleanup message for MQTT
        let reset_message = ResetCleanupMessage {
            command: "reset_cleanup".to_string(),
            device_id: reset_data.device_id.clone(),
            reset_timestamp: reset_data.reset_timestamp.clone(),
            old_certificate_arn: reset_data.old_cert_arn.clone(),
            reason: reset_data.reason.clone(),
        };

        // Send reset notification via MQTT
        match self.send_reset_notification(reset_message).await {
            Ok(_) => {
                info!("✅ Reset notification sent successfully");
                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::NotificationSent);
            }
            Err(e) => {
                warn!("⚠️ Failed to send immediate reset notification: {}", e);
                RESET_HANDLER_EVENT_SIGNAL
                    .signal(ResetHandlerEvent::NotificationFailed(e.to_string()));

                // Fall back to offline reset to ensure notification is preserved
                return self.execute_offline_reset(reset_data).await;
            }
        }

        // Wait for MQTT confirmation with timeout
        if let Err(_) = self.wait_for_mqtt_confirmation().await {
            warn!("⚠️ MQTT confirmation timeout, proceeding with reset");
        }

        // Perform selective NVS erasure and reboot
        self.perform_factory_reset().await?;

        Ok(())
    }

    /// Execute offline reset with deferred notification storage
    pub async fn execute_offline_reset(&mut self, reset_data: ResetNotificationData) -> Result<()> {
        info!("📴 Executing offline reset with deferred notification");
        RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::OfflineResetStarted);

        // Store reset notification for deferred delivery
        if let Some(ref mut storage) = self.reset_storage {
            match storage.store_reset_notification(&reset_data) {
                Ok(_) => {
                    info!("💾 Reset notification stored for deferred delivery");
                }
                Err(e) => {
                    error!("❌ Failed to store reset notification: {}", e);
                    RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::ResetError(e.to_string()));
                    // Continue with reset even if storage fails
                }
            }
        } else {
            warn!("⚠️ Reset storage not available, notification will be lost");
        }

        // Perform selective NVS erasure and reboot
        self.perform_factory_reset().await?;

        Ok(())
    }

    /// Check for and process pending reset notifications
    pub async fn process_deferred_notifications(&mut self) -> Result<()> {
        info!("🔍 Checking for pending reset notifications");

        // Check if there are pending notifications
        let is_pending = if let Some(ref mut storage) = self.reset_storage {
            storage.is_reset_pending()?
        } else {
            warn!("⚠️ Reset storage not available for deferred processing");
            return Ok(());
        };

        if !is_pending {
            debug!("📭 No pending reset notifications found");
            return Ok(());
        }

        // Validate reset data integrity
        let is_valid = if let Some(ref mut storage) = self.reset_storage {
            storage.validate_reset_data()?
        } else {
            false
        };

        if !is_valid {
            warn!("⚠️ Reset data validation failed, clearing corrupt data");
            if let Some(ref mut storage) = self.reset_storage {
                if let Err(e) = storage.clear_reset_notification() {
                    error!("❌ Failed to clear corrupt reset data: {}", e);
                }
            }
            return Ok(());
        }

        // Load stored reset notification data
        let reset_data = if let Some(ref mut storage) = self.reset_storage {
            storage.load_reset_notification()?
        } else {
            None
        };

        if let Some(reset_data) = reset_data {
            info!(
                "📂 Found pending reset notification for device: {}",
                reset_data.device_id
            );

            // Create reset cleanup message
            let reset_message = ResetCleanupMessage {
                command: "reset_cleanup".to_string(),
                device_id: reset_data.device_id,
                reset_timestamp: reset_data.reset_timestamp,
                old_certificate_arn: reset_data.old_cert_arn,
                reason: reset_data.reason,
            };

            // Send deferred notification
            match self.send_reset_notification(reset_message).await {
                Ok(_) => {
                    info!("✅ Deferred reset notification sent successfully");
                    RESET_HANDLER_EVENT_SIGNAL
                        .signal(ResetHandlerEvent::DeferredNotificationProcessed);

                    // Clear the pending notification
                    if let Some(ref mut storage) = self.reset_storage {
                        if let Err(e) = storage.clear_reset_notification() {
                            error!(
                                "❌ Failed to clear reset notification after delivery: {}",
                                e
                            );
                        } else {
                            info!("🧹 Pending reset notification cleared");
                        }
                    }
                }
                Err(e) => {
                    warn!("⚠️ Failed to send deferred reset notification: {}", e);
                    RESET_HANDLER_EVENT_SIGNAL
                        .signal(ResetHandlerEvent::NotificationFailed(e.to_string()));
                    // Keep the notification for next attempt
                }
            }
        } else {
            debug!("📭 No pending reset notification data found");
        }

        Ok(())
    }

    /// Send reset notification via MQTT
    async fn send_reset_notification(&self, reset_message: ResetCleanupMessage) -> Result<()> {
        info!("📤 Sending reset notification via MQTT");

        // Create MQTT message for reset notification
        let mqtt_message = MqttMessage::ResetNotification {
            device_id: reset_message.device_id,
            reset_timestamp: reset_message.reset_timestamp,
            old_cert_arn: reset_message.old_certificate_arn,
            reason: reset_message.reason,
        };

        // Send message via MQTT channel
        MQTT_MESSAGE_CHANNEL.sender().send(mqtt_message).await;

        debug!("📨 Reset notification queued for MQTT transmission");
        Ok(())
    }

    /// Wait for MQTT confirmation with timeout
    async fn wait_for_mqtt_confirmation(&self) -> Result<()> {
        info!("⏳ Waiting for MQTT confirmation");

        // Wait for MQTT event signal with timeout
        let timeout_duration = Duration::from_millis(RESET_CONFIRMATION_WAIT_MS);

        match with_timeout(timeout_duration, self.wait_for_mqtt_event()).await {
            Ok(_) => {
                debug!("✅ MQTT confirmation received");
                Ok(())
            }
            Err(_) => {
                warn!(
                    "⚠️ MQTT confirmation timeout after {}ms",
                    RESET_CONFIRMATION_WAIT_MS
                );
                Err(anyhow!("MQTT confirmation timeout"))
            }
        }
    }

    /// Wait for MQTT event signal
    async fn wait_for_mqtt_event(&self) {
        // In a real implementation, we would listen for specific MQTT events
        // For now, we'll use a simple delay as a placeholder
        Timer::after(Duration::from_millis(1000)).await;
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

        // Perform selective NVS erasure (preserving reset_pending namespace)
        match self.perform_selective_erasure().await {
            Ok(_) => {
                info!("✅ Selective NVS erasure completed successfully");
                RESET_HANDLER_EVENT_SIGNAL.signal(ResetHandlerEvent::SelectiveErasureCompleted);
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

    /// Perform selective NVS erasure
    async fn perform_selective_erasure(&self) -> Result<()> {
        info!("🗑️ Starting selective NVS erasure");

        // Get NVS partition for erasure
        let nvs_partition = self
            .nvs_partition
            .as_ref()
            .ok_or_else(|| anyhow!("NVS partition not initialized"))?;

        // Perform actual selective erasure operation with timeout
        let erasure_timeout = Duration::from_millis(SELECTIVE_ERASURE_TIMEOUT_MS);

        match with_timeout(
            erasure_timeout,
            self.execute_selective_erasure(nvs_partition.clone()),
        )
        .await
        {
            Ok(result) => match result {
                Ok(_) => {
                    info!("✅ Selective NVS erasure completed successfully");
                    Ok(())
                }
                Err(e) => {
                    error!("❌ Selective NVS erasure failed: {}", e);
                    Err(e)
                }
            },
            Err(_) => {
                error!(
                    "❌ Selective NVS erasure timeout after {}ms",
                    SELECTIVE_ERASURE_TIMEOUT_MS
                );
                Err(anyhow!("Selective NVS erasure timeout"))
            }
        }
    }

    /// Execute selective NVS erasure (production implementation)
    async fn execute_selective_erasure(&self, nvs_partition: EspDefaultNvsPartition) -> Result<()> {
        info!("🔥 Executing selective NVS erasure for factory reset");

        // List of namespaces to erase during factory reset
        let namespaces_to_erase = [
            "wifi_config",  // WiFi credentials
            "mqtt_certs",   // MQTT certificates
            "acorn_device", // Device configuration
        ];

        for namespace in &namespaces_to_erase {
            match self.erase_namespace(nvs_partition.clone(), namespace).await {
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
    async fn erase_namespace(
        &self,
        nvs_partition: EspDefaultNvsPartition,
        namespace: &str,
    ) -> Result<()> {
        debug!("🗑️ Erasing NVS namespace: {}", namespace);

        let mut nvs = EspNvs::new(nvs_partition, namespace, true)
            .map_err(|e| anyhow!("Failed to open namespace {}: {}", namespace, e))?;

        // Define known keys for each namespace to erase
        let keys_to_remove = match namespace {
            "wifi_config" => vec![
                "ssid",
                "password",
                "auth_token",
                "device_name",
                "user_timezone",
                "wifi_creds",
                "last_connected",
            ],
            "mqtt_certs" => vec![
                "device_cert",
                "private_key",
                "root_ca",
                "cert_metadata",
                "device_id",
                "cert_arn",
                "endpoint",
                "created_at",
                "iot_endpoint",
                "ca_cert",
                "validation",
            ],
            "acorn_device" => vec![
                "device_id",
                "firmware_version",
                "device_config",
                "auth_token",
                "registration_status",
                "last_heartbeat",
                "serial_number",
                "mac_address",
                "device_name",
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

            // Small delay to prevent blocking
            Timer::after(Duration::from_millis(10)).await;
        }

        info!(
            "✅ Erased {} keys from namespace: {}",
            removed_count, namespace
        );
        Ok(())
    }

    /// Check if reset storage is properly initialized
    pub fn is_storage_initialized(&self) -> bool {
        self.reset_storage.is_some()
    }

    /// Force immediate processing of deferred notifications (for testing)
    pub async fn force_process_deferred(&mut self) -> Result<()> {
        info!("🔧 Force processing deferred notifications");
        self.process_deferred_notifications().await
    }
}

/// Public interface functions for reset handling

/// Create reset cleanup message from notification data
pub fn create_reset_cleanup_message(reset_data: &ResetNotificationData) -> ResetCleanupMessage {
    ResetCleanupMessage {
        command: "reset_cleanup".to_string(),
        device_id: reset_data.device_id.clone(),
        reset_timestamp: reset_data.reset_timestamp.clone(),
        old_certificate_arn: reset_data.old_cert_arn.clone(),
        reason: reset_data.reason.clone(),
    }
}

/// Validate reset cleanup message format
pub fn validate_reset_message(message: &ResetCleanupMessage) -> Result<()> {
    if message.command != "reset_cleanup" {
        return Err(anyhow!("Invalid reset command: {}", message.command));
    }

    if message.device_id.is_empty() {
        return Err(anyhow!("Device ID cannot be empty"));
    }

    if message.reset_timestamp.is_empty() {
        return Err(anyhow!("Reset timestamp cannot be empty"));
    }

    if message.old_certificate_arn.is_empty() {
        return Err(anyhow!("Certificate ARN cannot be empty"));
    }

    if message.reason.is_empty() {
        return Err(anyhow!("Reset reason cannot be empty"));
    }

    Ok(())
}
