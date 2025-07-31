// Settings Management Module
// Production-grade settings management with MQTT integration and NVS persistence
// Follows Embassy async patterns and existing codebase architecture

// Import ESP-IDF's NVS (Non-Volatile Storage) functionality
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

// Import Embassy synchronization primitives for async coordination
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;

// Import Embassy time utilities
use embassy_time::{Duration, Timer};

// Import logging macros with consistent emoji prefixes
use log::{debug, error, info, warn};

// Import anyhow for error handling following existing patterns
use anyhow::{anyhow, Result};

// Import serde for JSON message parsing
use serde::{Deserialize, Serialize};

// Import standard library components (currently unused)
// use std::str::FromStr;

// Settings configuration constants
const NVS_NAMESPACE: &str = "device_settings"; // Namespace for device settings
const SETTINGS_CHANNEL_SIZE: usize = 16; // Channel capacity for settings updates
const SETTINGS_APPLY_TIMEOUT_SECONDS: u64 = 5; // Timeout for applying settings

// NVS storage keys for individual settings
const SOUND_ENABLED_KEY: &str = "sound_enabled";
const SOUND_VOLUME_KEY: &str = "sound_volume";
const LED_BRIGHTNESS_KEY: &str = "led_brightness";
const NOTIFICATION_COOLDOWN_KEY: &str = "notification_cooldown";
const QUIET_HOURS_ENABLED_KEY: &str = "quiet_hours_enabled";
const QUIET_HOURS_START_KEY: &str = "quiet_hours_start";
const QUIET_HOURS_END_KEY: &str = "quiet_hours_end";

/// Device settings structure based on technical documentation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSettings {
    pub sound_enabled: bool,        // Whether device makes sounds
    pub sound_volume: u8,           // Sound volume (1-10 scale)
    pub led_brightness: u8,         // LED brightness (1-10 scale)
    pub notification_cooldown: u32, // Seconds between notifications
    pub quiet_hours_enabled: bool,  // Whether quiet hours are active
    pub quiet_hours_start: String,  // Time format "HH:MM"
    pub quiet_hours_end: String,    // Time format "HH:MM"
}

impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            sound_volume: 5,
            led_brightness: 5,
            notification_cooldown: 5,
            quiet_hours_enabled: false,
            quiet_hours_start: "22:00".to_string(),
            quiet_hours_end: "07:00".to_string(),
        }
    }
}

impl DeviceSettings {
    /// Validate settings values are within acceptable ranges
    pub fn validate(&self) -> Result<()> {
        if self.sound_volume < 1 || self.sound_volume > 10 {
            return Err(anyhow!("Sound volume must be between 1 and 10"));
        }

        if self.led_brightness < 1 || self.led_brightness > 10 {
            return Err(anyhow!("LED brightness must be between 1 and 10"));
        }

        if self.notification_cooldown > 3600 {
            return Err(anyhow!("Notification cooldown cannot exceed 3600 seconds"));
        }

        // Validate time format for quiet hours
        self.validate_time_format(&self.quiet_hours_start, "quiet_hours_start")?;
        self.validate_time_format(&self.quiet_hours_end, "quiet_hours_end")?;

        Ok(())
    }

    /// Validate time format is "HH:MM"
    fn validate_time_format(&self, time_str: &str, field_name: &str) -> Result<()> {
        let parts: Vec<&str> = time_str.split(':').collect();
        if parts.len() != 2 {
            return Err(anyhow!("{} must be in HH:MM format", field_name));
        }

        let hours: u8 = parts[0]
            .parse()
            .map_err(|_| anyhow!("{} hours must be a valid number", field_name))?;
        let minutes: u8 = parts[1]
            .parse()
            .map_err(|_| anyhow!("{} minutes must be a valid number", field_name))?;

        if hours > 23 {
            return Err(anyhow!("{} hours must be 0-23", field_name));
        }
        if minutes > 59 {
            return Err(anyhow!("{} minutes must be 0-59", field_name));
        }

        Ok(())
    }

    /// Check if current time is within quiet hours
    pub fn is_quiet_hours_active(&self) -> bool {
        if !self.quiet_hours_enabled {
            return false;
        }

        // For MVP, return false - would need RTC integration for production
        // This could be enhanced with actual time checking
        false
    }
}

/// MQTT settings message format (incoming from AWS IoT Core)
#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    #[serde(rename = "volume")]
    pub sound_volume: Option<u8>,
    #[serde(rename = "ringerType")]
    pub ringer_type: Option<String>, // Not used in MVP, kept for compatibility
    #[serde(rename = "ringerDuration")]
    pub ringer_duration: Option<u8>, // Not used in MVP, kept for compatibility
    #[serde(rename = "cooldownPeriod")]
    pub notification_cooldown: Option<u32>,
    #[serde(rename = "ledBrightness")]
    pub led_brightness: Option<u8>,
    #[serde(rename = "soundEnabled")]
    pub sound_enabled: Option<bool>,
    #[serde(rename = "quietHoursEnabled")]
    pub quiet_hours_enabled: Option<bool>,
    #[serde(rename = "quietHoursStart")]
    pub quiet_hours_start: Option<String>,
    #[serde(rename = "quietHoursEnd")]
    pub quiet_hours_end: Option<String>,
}

/// Settings events for external coordination
#[derive(Debug, Clone)]
pub enum SettingsEvent {
    SettingsUpdated(DeviceSettings),
    SettingsLoadError(String),
    SettingsSaveError(String),
    InvalidSettings(String),
    MqttSettingsReceived(String), // JSON payload
}

/// Settings update request for internal communication
#[derive(Debug, Clone)]
pub enum SettingsRequest {
    LoadSettings,
    SaveSettings(DeviceSettings),
    ApplyMqttUpdate(String), // JSON payload
    GetCurrentSettings,
}

// Global settings communication channels
pub static SETTINGS_REQUEST_CHANNEL: Channel<
    CriticalSectionRawMutex,
    SettingsRequest,
    SETTINGS_CHANNEL_SIZE,
> = Channel::new();

pub static SETTINGS_EVENT_SIGNAL: Signal<CriticalSectionRawMutex, SettingsEvent> = Signal::new();

/// Settings manager - handles NVS storage and MQTT integration
pub struct SettingsManager {
    nvs: EspNvs<NvsDefault>,
    current_settings: DeviceSettings,
    device_id: String,
}

impl SettingsManager {
    /// Create new settings manager with NVS partition
    pub fn new_with_partition(
        nvs_partition: EspDefaultNvsPartition,
        device_id: String,
    ) -> Result<Self> {
        info!("🔧 Initializing settings manager for device: {}", device_id);

        // Open the NVS namespace for device settings
        let nvs = EspNvs::new(nvs_partition, NVS_NAMESPACE, true)
            .map_err(|e| anyhow!("Failed to open settings NVS namespace: {}", e))?;

        let mut manager = Self {
            nvs,
            current_settings: DeviceSettings::default(),
            device_id,
        };

        // Load existing settings or use defaults
        match manager.load_settings_from_nvs() {
            Ok(settings) => {
                manager.current_settings = settings;
                info!("✅ Settings loaded from NVS");
            }
            Err(e) => {
                warn!("⚠️ Could not load settings from NVS: {}, using defaults", e);
                // Save defaults to NVS
                if let Err(save_err) =
                    manager.save_settings_to_nvs(&manager.current_settings.clone())
                {
                    warn!("⚠️ Failed to save default settings: {}", save_err);
                } else {
                    info!("✅ Default settings saved to NVS");
                }
            }
        }

        info!("🔧 Settings manager initialized successfully");
        Ok(manager)
    }

    /// Embassy task for handling settings requests
    pub async fn run_settings_task(&mut self) {
        info!("🔧 Starting settings management task");

        loop {
            // Wait for settings requests
            let request = SETTINGS_REQUEST_CHANNEL.receive().await;
            debug!("🔧 Processing settings request: {:?}", request);

            let result = match request {
                SettingsRequest::LoadSettings => self.handle_load_settings().await,
                SettingsRequest::SaveSettings(settings) => {
                    self.handle_save_settings(settings).await
                }
                SettingsRequest::ApplyMqttUpdate(json_payload) => {
                    self.handle_mqtt_settings_update(json_payload).await
                }
                SettingsRequest::GetCurrentSettings => {
                    // Signal current settings
                    SETTINGS_EVENT_SIGNAL.signal(SettingsEvent::SettingsUpdated(
                        self.current_settings.clone(),
                    ));
                    Ok(())
                }
            };

            if let Err(e) = result {
                error!("❌ Settings request failed: {}", e);
            }

            // Small delay to prevent busy waiting
            Timer::after(Duration::from_millis(10)).await;
        }
    }

    /// Handle loading settings from NVS
    async fn handle_load_settings(&mut self) -> Result<()> {
        match self.load_settings_from_nvs() {
            Ok(settings) => {
                self.current_settings = settings.clone();
                SETTINGS_EVENT_SIGNAL.signal(SettingsEvent::SettingsUpdated(settings));
                info!("✅ Settings loaded successfully");
                Ok(())
            }
            Err(e) => {
                let error_msg = format!("Failed to load settings: {}", e);
                SETTINGS_EVENT_SIGNAL.signal(SettingsEvent::SettingsLoadError(error_msg.clone()));
                Err(anyhow!(error_msg))
            }
        }
    }

    /// Handle saving settings to NVS
    async fn handle_save_settings(&mut self, settings: DeviceSettings) -> Result<()> {
        // Validate settings before saving
        if let Err(e) = settings.validate() {
            let error_msg = format!("Invalid settings: {}", e);
            SETTINGS_EVENT_SIGNAL.signal(SettingsEvent::InvalidSettings(error_msg.clone()));
            return Err(anyhow!(error_msg));
        }

        match self.save_settings_to_nvs(&settings) {
            Ok(()) => {
                self.current_settings = settings.clone();
                SETTINGS_EVENT_SIGNAL.signal(SettingsEvent::SettingsUpdated(settings));
                info!("✅ Settings saved successfully");
                Ok(())
            }
            Err(e) => {
                let error_msg = format!("Failed to save settings: {}", e);
                SETTINGS_EVENT_SIGNAL.signal(SettingsEvent::SettingsSaveError(error_msg.clone()));
                Err(anyhow!(error_msg))
            }
        }
    }

    /// Handle MQTT settings update
    async fn handle_mqtt_settings_update(&mut self, json_payload: String) -> Result<()> {
        info!("🔧 Processing MQTT settings update");
        debug!("📨 MQTT settings payload: {}", json_payload);

        // Signal that MQTT settings were received
        SETTINGS_EVENT_SIGNAL.signal(SettingsEvent::MqttSettingsReceived(json_payload.clone()));

        // Parse the JSON payload
        let update: SettingsUpdate = serde_json::from_str(&json_payload)
            .map_err(|e| anyhow!("Failed to parse settings JSON: {}", e))?;

        // Apply updates to current settings
        let mut new_settings = self.current_settings.clone();

        if let Some(volume) = update.sound_volume {
            new_settings.sound_volume = volume;
            debug!("🔊 Updated sound volume: {}", volume);
        }

        if let Some(enabled) = update.sound_enabled {
            new_settings.sound_enabled = enabled;
            debug!("🔊 Updated sound enabled: {}", enabled);
        }

        if let Some(brightness) = update.led_brightness {
            new_settings.led_brightness = brightness;
            debug!("💡 Updated LED brightness: {}", brightness);
        }

        if let Some(cooldown) = update.notification_cooldown {
            new_settings.notification_cooldown = cooldown;
            debug!("⏱️ Updated notification cooldown: {}s", cooldown);
        }

        if let Some(enabled) = update.quiet_hours_enabled {
            new_settings.quiet_hours_enabled = enabled;
            debug!("🌙 Updated quiet hours enabled: {}", enabled);
        }

        if let Some(start) = update.quiet_hours_start {
            new_settings.quiet_hours_start = start.clone();
            debug!("🌙 Updated quiet hours start: {}", start);
        }

        if let Some(end) = update.quiet_hours_end {
            new_settings.quiet_hours_end = end.clone();
            debug!("🌙 Updated quiet hours end: {}", end);
        }

        // Validate and save the updated settings
        if let Err(e) = new_settings.validate() {
            let error_msg = format!("Invalid MQTT settings update: {}", e);
            SETTINGS_EVENT_SIGNAL.signal(SettingsEvent::InvalidSettings(error_msg.clone()));
            return Err(anyhow!(error_msg));
        }

        // Save to NVS and update current settings
        self.save_settings_to_nvs(&new_settings)?;
        self.current_settings = new_settings.clone();

        SETTINGS_EVENT_SIGNAL.signal(SettingsEvent::SettingsUpdated(new_settings));
        info!("✅ MQTT settings update applied successfully");

        Ok(())
    }

    /// Load settings from NVS storage
    fn load_settings_from_nvs(&mut self) -> Result<DeviceSettings> {
        debug!("🔧 Loading settings from NVS");

        let sound_enabled = self
            .get_bool_setting(SOUND_ENABLED_KEY)
            .unwrap_or(DeviceSettings::default().sound_enabled);

        let sound_volume = self
            .get_u8_setting(SOUND_VOLUME_KEY)
            .unwrap_or(DeviceSettings::default().sound_volume);

        let led_brightness = self
            .get_u8_setting(LED_BRIGHTNESS_KEY)
            .unwrap_or(DeviceSettings::default().led_brightness);

        let notification_cooldown = self
            .get_u32_setting(NOTIFICATION_COOLDOWN_KEY)
            .unwrap_or(DeviceSettings::default().notification_cooldown);

        let quiet_hours_enabled = self
            .get_bool_setting(QUIET_HOURS_ENABLED_KEY)
            .unwrap_or(DeviceSettings::default().quiet_hours_enabled);

        let quiet_hours_start = self
            .get_string_setting(QUIET_HOURS_START_KEY)
            .unwrap_or(DeviceSettings::default().quiet_hours_start);

        let quiet_hours_end = self
            .get_string_setting(QUIET_HOURS_END_KEY)
            .unwrap_or(DeviceSettings::default().quiet_hours_end);

        let settings = DeviceSettings {
            sound_enabled,
            sound_volume,
            led_brightness,
            notification_cooldown,
            quiet_hours_enabled,
            quiet_hours_start,
            quiet_hours_end,
        };

        // Validate loaded settings
        settings.validate()?;

        debug!("🔧 Settings loaded from NVS: {:?}", settings);
        Ok(settings)
    }

    /// Save settings to NVS storage
    fn save_settings_to_nvs(&mut self, settings: &DeviceSettings) -> Result<()> {
        debug!("🔧 Saving settings to NVS: {:?}", settings);

        self.nvs
            .set_u8(
                SOUND_ENABLED_KEY,
                if settings.sound_enabled { 1 } else { 0 },
            )
            .map_err(|e| anyhow!("Failed to save sound_enabled: {}", e))?;

        self.nvs
            .set_u8(SOUND_VOLUME_KEY, settings.sound_volume)
            .map_err(|e| anyhow!("Failed to save sound_volume: {}", e))?;

        self.nvs
            .set_u8(LED_BRIGHTNESS_KEY, settings.led_brightness)
            .map_err(|e| anyhow!("Failed to save led_brightness: {}", e))?;

        self.nvs
            .set_u32(NOTIFICATION_COOLDOWN_KEY, settings.notification_cooldown)
            .map_err(|e| anyhow!("Failed to save notification_cooldown: {}", e))?;

        self.nvs
            .set_u8(
                QUIET_HOURS_ENABLED_KEY,
                if settings.quiet_hours_enabled { 1 } else { 0 },
            )
            .map_err(|e| anyhow!("Failed to save quiet_hours_enabled: {}", e))?;

        self.nvs
            .set_str(QUIET_HOURS_START_KEY, &settings.quiet_hours_start)
            .map_err(|e| anyhow!("Failed to save quiet_hours_start: {}", e))?;

        self.nvs
            .set_str(QUIET_HOURS_END_KEY, &settings.quiet_hours_end)
            .map_err(|e| anyhow!("Failed to save quiet_hours_end: {}", e))?;

        debug!("✅ Settings saved to NVS successfully");
        Ok(())
    }

    /// Helper method to get boolean setting from NVS
    fn get_bool_setting(&mut self, key: &str) -> Option<bool> {
        match self.nvs.get_u8(key) {
            Ok(Some(value)) => Some(value != 0),
            _ => None,
        }
    }

    /// Helper method to get u8 setting from NVS
    fn get_u8_setting(&mut self, key: &str) -> Option<u8> {
        match self.nvs.get_u8(key) {
            Ok(Some(value)) => Some(value),
            _ => None,
        }
    }

    /// Helper method to get u32 setting from NVS
    fn get_u32_setting(&mut self, key: &str) -> Option<u32> {
        match self.nvs.get_u32(key) {
            Ok(Some(value)) => Some(value),
            _ => None,
        }
    }

    /// Helper method to get string setting from NVS
    fn get_string_setting(&mut self, key: &str) -> Option<String> {
        let mut buffer = [0u8; 64]; // Sufficient for time format "HH:MM"
        match self.nvs.get_str(key, &mut buffer) {
            Ok(Some(str_value)) => Some(str_value.to_string()),
            _ => None,
        }
    }

    /// Get current settings (non-async access)
    pub fn get_current_settings(&self) -> &DeviceSettings {
        &self.current_settings
    }

    /// Get device ID
    pub fn get_device_id(&self) -> &str {
        &self.device_id
    }
}

/// Utility functions for external integration

/// Send settings request (non-blocking)
pub fn request_settings_load() {
    if let Err(_) = SETTINGS_REQUEST_CHANNEL.try_send(SettingsRequest::LoadSettings) {
        warn!("⚠️ Settings request channel full, skipping load request");
    }
}

/// Send settings save request (non-blocking)
pub fn request_settings_save(settings: DeviceSettings) {
    if let Err(_) = SETTINGS_REQUEST_CHANNEL.try_send(SettingsRequest::SaveSettings(settings)) {
        warn!("⚠️ Settings request channel full, skipping save request");
    }
}

/// Send MQTT settings update request (non-blocking)
pub fn request_mqtt_settings_update(json_payload: String) {
    if let Err(_) =
        SETTINGS_REQUEST_CHANNEL.try_send(SettingsRequest::ApplyMqttUpdate(json_payload))
    {
        warn!("⚠️ Settings request channel full, skipping MQTT update");
    }
}

/// Get current settings request (non-blocking)
pub fn request_current_settings() {
    if let Err(_) = SETTINGS_REQUEST_CHANNEL.try_send(SettingsRequest::GetCurrentSettings) {
        warn!("⚠️ Settings request channel full, skipping current settings request");
    }
}
