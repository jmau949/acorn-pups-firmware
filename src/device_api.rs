use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::ApiClient;

/// Device registration request payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRegistration {
    /// Unique device identifier (UUID)
    pub device_id: String,
    /// Hardware serial number (unique)
    pub serial_number: String,
    /// Device MAC address
    pub mac_address: String,
    /// User-friendly device name
    pub device_name: String,
    /// Current firmware version
    pub firmware_version: String,
    /// Hardware version/revision
    pub hardware_version: String,
    /// Device type identifier
    pub device_type: String,
    /// WiFi network SSID the device is connected to
    pub wifi_ssid: String,
    /// WiFi signal strength in dBm
    pub signal_strength: i32,
}

/// Device registration response from the backend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRegistrationResponse {
    /// Confirmed device ID
    pub device_id: String,
    /// AWS IoT Core device certificate (X.509 PEM format)
    pub certificate: String,
    /// Device private key (RSA PEM format)
    pub private_key: String,
    /// AWS IoT Core endpoint URL
    pub iot_endpoint: String,
    /// AWS IoT Thing name
    pub iot_thing_name: String,
    /// MQTT topics the device should use
    pub mqtt_topics: MqttTopics,
    /// Device configuration settings
    pub device_settings: DeviceSettings,
}

/// MQTT topic configuration for the device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttTopics {
    /// Topic for publishing button press events
    pub button_press_topic: String,
    /// Topic for publishing device status updates
    pub status_topic: String,
    /// Topic for subscribing to device settings updates
    pub settings_topic: String,
    /// Topic for subscribing to device commands
    pub commands_topic: String,
}

/// Device settings configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSettings {
    /// Device sound volume (1-10)
    pub sound_volume: u8,
    /// LED brightness level (1-10)
    pub led_brightness: u8,
    /// Notification cooldown period in seconds
    pub notification_cooldown: u32,
    /// Whether sound alerts are enabled
    pub sound_enabled: bool,
    /// Whether quiet hours are enabled
    pub quiet_hours_enabled: bool,
    /// Quiet hours start time (HH:MM format)
    pub quiet_hours_start: Option<String>,
    /// Quiet hours end time (HH:MM format)
    pub quiet_hours_end: Option<String>,
}

/// Device reset request payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceResetRequest {
    /// Device ID being reset
    pub device_id: String,
    /// Reason for the reset
    pub reason: String,
    /// Timestamp when reset was initiated
    pub timestamp: u64,
    /// Firmware version at time of reset
    pub firmware_version: String,
    /// Optional additional reset context
    pub context: Option<String>,
}

/// Device reset response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceResetResponse {
    /// Confirmation of reset processing
    pub success: bool,
    /// Reset acknowledgment message
    pub message: String,
    /// Timestamp of reset processing
    pub processed_at: u64,
}

/// Device status update payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatusUpdate {
    /// Device ID reporting status
    pub device_id: String,
    /// Current online status
    pub is_online: bool,
    /// Current WiFi signal strength
    pub signal_strength: i32,
    /// Device uptime in seconds
    pub uptime: u64,
    /// Memory usage percentage
    pub memory_usage: f32,
    /// CPU temperature in Celsius
    pub cpu_temperature: f32,
    /// Current firmware version
    pub firmware_version: String,
    /// Timestamp of status report
    pub timestamp: u64,
}

/// Device-specific API client for ESP32 receivers
pub struct DeviceApiClient {
    /// Underlying HTTP API client
    api_client: ApiClient,
    /// Device ID for this instance
    device_id: String,
    /// Current firmware version
    firmware_version: String,
}

impl DeviceApiClient {
    /// Create a new device API client
    pub fn new(base_url: String, device_id: String, firmware_version: String) -> Self {
        let api_client = ApiClient::new(base_url);
        
        info!("Initialized device API client for device: {}", device_id);
        
        Self {
            api_client,
            device_id,
            firmware_version,
        }
    }

    /// Set the JWT authentication token (from AWS Cognito)
    pub async fn set_auth_token(&self, token: String) {
        debug!("Setting authentication token for device API client");
        self.api_client.set_token(token).await;
    }

    /// Clear the authentication token
    pub async fn clear_auth_token(&self) {
        debug!("Clearing authentication token");
        self.api_client.clear_token().await;
    }

    /// Register this device with the backend API
    /// 
    /// This method is authenticated and requires a valid JWT token
    /// from AWS Cognito to be set via set_auth_token()
    pub async fn register_device(&self, device_info: &DeviceRegistration) -> Result<DeviceRegistrationResponse> {
        info!("Registering device: {}", device_info.device_id);
        
        // Validate device info
        if device_info.device_id.is_empty() {
            return Err(anyhow!("Device ID cannot be empty"));
        }
        
        if device_info.serial_number.is_empty() {
            return Err(anyhow!("Serial number cannot be empty"));
        }
        
        if device_info.mac_address.is_empty() {
            return Err(anyhow!("MAC address cannot be empty"));
        }

        debug!("Device registration details: {:?}", device_info);
        
        // Make authenticated API call
        match self.api_client.post("/devices/register", device_info).await {
            Ok(response) => {
                // Parse the registration response
                match serde_json::from_str::<DeviceRegistrationResponse>(&response.body) {
                    Ok(registration_response) => {
                        info!("Device registered successfully: {}", registration_response.device_id);
                        info!("IoT endpoint: {}", registration_response.iot_endpoint);
                        info!("IoT thing name: {}", registration_response.iot_thing_name);
                        
                        // Log MQTT topics for debugging
                        debug!("MQTT topics configured:");
                        debug!("  Button press: {}", registration_response.mqtt_topics.button_press_topic);
                        debug!("  Status: {}", registration_response.mqtt_topics.status_topic);
                        debug!("  Settings: {}", registration_response.mqtt_topics.settings_topic);
                        debug!("  Commands: {}", registration_response.mqtt_topics.commands_topic);
                        
                        Ok(registration_response)
                    }
                    Err(e) => {
                        error!("Failed to parse device registration response: {}", e);
                        error!("Response body: {}", response.body);
                        Err(anyhow!("Invalid registration response format: {}", e))
                    }
                }
            }
            Err(e) => {
                error!("Device registration failed: {}", e);
                Err(anyhow!("Device registration failed: {}", e))
            }
        }
    }

    /// Reset device and notify backend
    /// 
    /// This method is unauthenticated and can be called without a token
    pub async fn reset_device(&self, reason: &str, context: Option<String>) -> Result<DeviceResetResponse> {
        info!("Initiating device reset. Reason: {}", reason);
        
        // Clear any existing auth token since this endpoint is unauthenticated
        self.api_client.clear_token().await;
        
        let reset_request = DeviceResetRequest {
            device_id: self.device_id.clone(),
            reason: reason.to_string(),
            timestamp: self.get_current_timestamp(),
            firmware_version: self.firmware_version.clone(),
            context,
        };
        
        debug!("Device reset request: {:?}", reset_request);
        
        let endpoint = format!("/devices/{}/reset", self.device_id);
        
        match self.api_client.post(&endpoint, &reset_request).await {
            Ok(response) => {
                match serde_json::from_str::<DeviceResetResponse>(&response.body) {
                    Ok(reset_response) => {
                        info!("Device reset processed successfully: {}", reset_response.message);
                        Ok(reset_response)
                    }
                    Err(e) => {
                        error!("Failed to parse device reset response: {}", e);
                        error!("Response body: {}", response.body);
                        Err(anyhow!("Invalid reset response format: {}", e))
                    }
                }
            }
            Err(e) => {
                error!("Device reset request failed: {}", e);
                Err(anyhow!("Device reset failed: {}", e))
            }
        }
    }


    /// Get device ID
    pub fn get_device_id(&self) -> &str {
        &self.device_id
    }

    /// Get firmware version
    pub fn get_firmware_version(&self) -> &str {
        &self.firmware_version
    }

    /// Update firmware version
    pub fn update_firmware_version(&mut self, version: String) {
        info!("Updating firmware version from {} to {}", self.firmware_version, version);
        self.firmware_version = version;
    }

    /// Get current Unix timestamp
    fn get_current_timestamp(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}



/// Helper functions for device information gathering
impl DeviceApiClient {
    /// Create a device registration payload with current system information
    pub fn create_device_registration(
        &self,
        serial_number: String,
        mac_address: String,
        device_name: String,
        hardware_version: String,
        wifi_ssid: String,
        signal_strength: i32,
    ) -> DeviceRegistration {
        DeviceRegistration {
            device_id: self.device_id.clone(),
            serial_number,
            mac_address,
            device_name,
            firmware_version: self.firmware_version.clone(),
            hardware_version,
            device_type: "acorn-pups-receiver".to_string(),
            wifi_ssid,
            signal_strength,
        }
    }


}

/// Default device settings
impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            sound_volume: 7,
            led_brightness: 5,
            notification_cooldown: 5,
            sound_enabled: true,
            quiet_hours_enabled: false,
            quiet_hours_start: None,
            quiet_hours_end: None,
        }
    }
}



 