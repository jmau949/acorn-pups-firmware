use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::ApiClient;

/// Device registration request payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRegistration {
    /// Unique device identifier
    #[serde(rename = "deviceId")]
    pub device_id: String,
    /// User-friendly device name
    #[serde(rename = "deviceName")]
    pub device_name: String,
    /// Hardware serial number (unique)
    #[serde(rename = "serialNumber")]
    pub serial_number: String,
    /// Device MAC address
    #[serde(rename = "macAddress")]
    pub mac_address: String,
}

/// Device registration response from the backend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRegistrationResponse {
    /// Device registration data
    pub data: DeviceRegistrationData,
    /// Request ID for tracking
    #[serde(rename = "requestId")]
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRegistrationData {
    /// Confirmed device ID
    #[serde(rename = "deviceId")]
    pub device_id: String,
    /// User-friendly device name
    #[serde(rename = "deviceName")]
    pub device_name: String,
    /// Hardware serial number
    #[serde(rename = "serialNumber")]
    pub serial_number: String,
    /// Device owner user ID
    #[serde(rename = "ownerId")]
    pub owner_id: String,
    /// Registration timestamp
    #[serde(rename = "registeredAt")]
    pub registered_at: String,
    /// Device status
    pub status: String,
    /// Device certificates and IoT configuration
    pub certificates: DeviceCertificates,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCertificates {
    /// AWS IoT Core device certificate (X.509 PEM format)
    #[serde(rename = "deviceCertificate")]
    pub device_certificate: String,
    /// Device private key (RSA PEM format)
    #[serde(rename = "privateKey")]
    pub private_key: String,
    /// AWS IoT Core endpoint URL
    #[serde(rename = "iotEndpoint")]
    pub iot_endpoint: String,
}

/// MQTT topic configuration for the device


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
    /// Device reset data
    pub data: DeviceResetData,
    /// Request ID for tracking
    #[serde(rename = "requestId")]
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceResetData {
    /// Device ID that was reset
    #[serde(rename = "deviceId")]
    pub device_id: String,
    /// Reset acknowledgment message
    pub message: String,
    /// Timestamp when reset was initiated
    #[serde(rename = "resetInitiatedAt")]
    pub reset_initiated_at: String,
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

        if device_info.device_name.is_empty() {
            return Err(anyhow!("Device name cannot be empty"));
        }

        debug!("Device registration details: {:?}", device_info);
        
        // Make authenticated API call
        match self.api_client.post("/devices/register", device_info).await {
            Ok(response) => {
                // Parse the registration response
                match serde_json::from_str::<DeviceRegistrationResponse>(&response.body) {
                    Ok(registration_response) => {
                        info!("Device registered successfully: {}", registration_response.data.device_id);
                        info!("Owner ID: {}", registration_response.data.owner_id);
                        info!("IoT endpoint: {}", registration_response.data.certificates.iot_endpoint);
                        info!("Registration timestamp: {}", registration_response.data.registered_at);
                        info!("Request ID: {}", registration_response.request_id);
                        
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
                        info!("Device reset processed successfully: {}", reset_response.data.message);
                        info!("Device ID: {}", reset_response.data.device_id);
                        info!("Reset initiated at: {}", reset_response.data.reset_initiated_at);
                        info!("Request ID: {}", reset_response.request_id);
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
    ) -> DeviceRegistration {
        DeviceRegistration {
            device_id: self.device_id.clone(),
            device_name,
            serial_number,
            mac_address,
        }
    }


}





 