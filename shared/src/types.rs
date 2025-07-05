use serde::{Deserialize, Serialize};

// WiFi connection status events
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WiFiConnectionEvent {
    ConnectionAttempting,                     // WiFi connection is being attempted
    ConnectionSuccessful(std::net::Ipv4Addr), // WiFi connected successfully with IP
    ConnectionFailed(String),                 // WiFi connection failed with error
    CredentialsStored,                        // WiFi credentials stored successfully
    CredentialsInvalid,                       // WiFi credentials validation failed
}

// System lifecycle events
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SystemEvent {
    SystemStartup,           // System has started and is initializing
    ProvisioningMode,        // Device is in WiFi provisioning mode via BLE
    WiFiMode,                // Device has transitioned to WiFi-only mode
    SystemError(String),     // System error occurred
    TaskTerminating(String), // Task is cleanly terminating
}

// System state structure
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemState {
    pub wifi_connected: bool,
    pub wifi_ip: Option<std::net::Ipv4Addr>,
    pub ble_active: bool,
    pub ble_client_connected: bool,
    pub provisioning_complete: bool,
}

impl SystemState {
    pub const fn new() -> Self {
        Self {
            wifi_connected: false,
            wifi_ip: None,
            ble_active: false,
            ble_client_connected: false,
            provisioning_complete: false,
        }
    }
}

// WiFi credentials structure
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WiFiCredentials {
    pub ssid: String,
    pub password: String,
}

impl WiFiCredentials {
    pub fn new(ssid: String, password: String) -> Self {
        Self { ssid, password }
    }
}

// Device information structure
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub device_type: String,
    pub firmware_version: String,
    pub hardware_platform: String,
}

impl DeviceInfo {
    pub fn new(device_id: String) -> Self {
        Self {
            device_id,
            device_type: "acorn_pups_esp32".to_string(),
            firmware_version: "1.0.0".to_string(),
            hardware_platform: "ESP32".to_string(),
        }
    }
}

// LED state enumeration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LedState {
    Red,   // System startup/default state
    Blue,  // BLE broadcasting/advertising
    Green, // Connected (BLE client or WiFi)
}

// Task execution modes
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ExecutionMode {
    Production,  // Real hardware with BLE/WiFi
    Development, // Wokwi simulator without BLE/WiFi
}
