// Import ESP-IDF's NVS (Non-Volatile Storage) functionality
// NVS is a key-value storage system that persists data in flash memory
// Data stored in NVS survives device reboots and power cycles
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

// Import ESP-IDF error type for operation results
use esp_idf_svc::sys::EspError;

// Import logging macros for debug output
use log::{error, info, warn};

// Import Serde traits for JSON serialization
// This allows us to save Rust structs as JSON strings in NVS
use serde::{Deserialize, Serialize};
// revert
// Import the WiFi credentials structure from our BLE module
use crate::ble_server::WiFiCredentials;

// NVS storage keys - these are the "names" we use to store/retrieve data
// NVS works like a dictionary where each piece of data has a unique key
const NVS_NAMESPACE: &str = "wifi_config"; // Namespace groups related keys together
const SSID_KEY: &str = "ssid"; // Key for storing WiFi network name
const PASSWORD_KEY: &str = "password"; // Key for storing WiFi password
const WIFI_CONFIG_KEY: &str = "wifi_creds"; // Key for storing complete WiFi config as JSON

// Extended WiFi configuration structure for storage
// This includes extra metadata beyond just the basic credentials
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredWiFiConfig {
    pub ssid: String,     // WiFi network name
    pub password: String, // WiFi password
    pub timestamp: u64,   // When this config was stored (Unix timestamp)
    pub is_valid: bool,   // Whether this config should be used
}

// WiFi storage manager - handles all NVS operations for WiFi credentials
// This struct wraps the ESP-IDF NVS functionality with WiFi-specific logic
pub struct WiFiStorage {
    nvs: EspNvs<NvsDefault>, // ESP-IDF NVS handle for flash storage operations
}

impl WiFiStorage {
    /// Create WiFi storage using provided NVS partition
    pub fn new_with_partition(nvs_partition: EspDefaultNvsPartition) -> Result<Self, EspError> {
        info!("Initializing WiFi storage with provided NVS partition");

        // Open the NVS namespace for WiFi configuration
        let nvs = EspNvs::new(nvs_partition, NVS_NAMESPACE, true)?;

        info!("WiFi storage initialized successfully");

        Ok(Self { nvs })
    }

    /// Create WiFi storage by taking the default NVS partition
    /// Note: Will fail if partition is already taken elsewhere
    pub fn new() -> Result<Self, EspError> {
        info!("Initializing WiFi storage with default NVS partition");

        let nvs_default_partition = EspDefaultNvsPartition::take()?;
        let nvs = EspNvs::new(nvs_default_partition, NVS_NAMESPACE, true)?;

        info!("WiFi storage initialized successfully");

        Ok(Self { nvs })
    }

    pub fn store_credentials(&mut self, credentials: &WiFiCredentials) -> Result<(), EspError> {
        info!("Storing WiFi credentials for SSID: {}", credentials.ssid);

        // Validate credentials before storing
        if !self.validate_credentials(credentials) {
            error!("Invalid credentials provided for storage");
            return Err(EspError::from_infallible::<
                { esp_idf_svc::sys::ESP_ERR_INVALID_ARG },
            >());
        }

        // Create stored config with metadata
        let stored_config = StoredWiFiConfig {
            ssid: credentials.ssid.clone(),
            password: credentials.password.clone(),
            timestamp: self.get_current_timestamp(),
            is_valid: true,
        };

        // Serialize the config to JSON
        match serde_json::to_string(&stored_config) {
            Ok(json_str) => {
                // Store the serialized config in NVS
                self.nvs.set_str(WIFI_CONFIG_KEY, &json_str)?;

                // Also store individual fields for backward compatibility
                self.nvs.set_str(SSID_KEY, &credentials.ssid)?;
                self.nvs.set_str(PASSWORD_KEY, &credentials.password)?;

                info!("WiFi credentials stored successfully in NVS");
                Ok(())
            }
            Err(e) => {
                error!("Failed to serialize WiFi config: {}", e);
                Err(EspError::from_infallible::<
                    { esp_idf_svc::sys::ESP_ERR_INVALID_ARG },
                >())
            }
        }
    }

    pub fn load_credentials(&mut self) -> Result<Option<WiFiCredentials>, EspError> {
        info!("Loading WiFi credentials from NVS");

        // Try to load the complete config first
        match self.nvs.get_str(WIFI_CONFIG_KEY, &mut [0u8; 512]) {
            Ok(Some(json_str)) => match serde_json::from_str::<StoredWiFiConfig>(&json_str) {
                Ok(config) if config.is_valid => {
                    info!("Loaded WiFi config for SSID: {}", config.ssid);
                    Ok(Some(WiFiCredentials {
                        ssid: config.ssid,
                        password: config.password,
                    }))
                }
                Ok(_) => {
                    warn!("Found invalid WiFi config in storage");
                    Ok(None)
                }
                Err(e) => {
                    warn!("Failed to parse stored WiFi config: {}", e);
                    self.load_credentials_fallback()
                }
            },
            Ok(None) => {
                info!("No WiFi config found in NVS, trying fallback method");
                self.load_credentials_fallback()
            }
            Err(e) => {
                warn!("Error reading WiFi config: {:?}, trying fallback", e);
                self.load_credentials_fallback()
            }
        }
    }

    fn load_credentials_fallback(&mut self) -> Result<Option<WiFiCredentials>, EspError> {
        // Try to load individual SSID and password fields
        let mut ssid_read_buffer = [0u8; 64];
        let mut password_read_buffer = [0u8; 64];

        let ssid = match self.nvs.get_str(SSID_KEY, &mut ssid_read_buffer)? {
            Some(ssid) if !ssid.is_empty() => ssid,
            _ => {
                info!("No SSID found in NVS storage");
                return Ok(None);
            }
        };

        let password = match self.nvs.get_str(PASSWORD_KEY, &mut password_read_buffer)? {
            Some(password) => password,
            None => {
                warn!("SSID found but no password in storage");
                return Ok(None);
            }
        };

        let credentials = WiFiCredentials {
            ssid: ssid.to_string(),
            password: password.to_string(),
        };

        if self.validate_credentials(&credentials) {
            info!("Loaded WiFi credentials from fallback storage");
            Ok(Some(credentials))
        } else {
            warn!("Invalid credentials found in storage");
            Ok(None)
        }
    }

    pub fn clear_credentials(&mut self) -> Result<(), EspError> {
        info!("Clearing stored WiFi credentials");

        // Remove all WiFi-related keys
        let _ = self.nvs.remove(WIFI_CONFIG_KEY);
        let _ = self.nvs.remove(SSID_KEY);
        let _ = self.nvs.remove(PASSWORD_KEY);

        info!("WiFi credentials cleared from storage");
        Ok(())
    }

    pub fn has_stored_credentials(&mut self) -> bool {
        // Check if we have any stored credentials
        match self.load_credentials() {
            Ok(Some(_)) => true,
            _ => false,
        }
    }

    fn validate_credentials(&self, credentials: &WiFiCredentials) -> bool {
        // SSID validation
        if credentials.ssid.is_empty() {
            warn!("SSID cannot be empty");
            return false;
        }

        if credentials.ssid.len() > 32 {
            warn!(
                "SSID too long: {} characters (max 32)",
                credentials.ssid.len()
            );
            return false;
        }

        // Password validation not required (allows open WiFi networks)

        // Log warning for non-ASCII SSIDs (allowed but may cause compatibility issues)
        if !credentials.ssid.is_ascii() {
            warn!("SSID contains non-ASCII characters - may cause compatibility issues");
        }

        true
    }

    fn get_current_timestamp(&self) -> u64 {
        // Get current Unix timestamp in seconds
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Debug method to dump all persistent storage contents
    pub fn debug_dump_storage(&mut self) {
        info!(
            "🔍 DEBUG: Dumping all persistent storage contents for namespace '{}'",
            NVS_NAMESPACE
        );

        // Try to read the complete WiFi config JSON
        let mut config_buffer = [0u8; 512];
        match self.nvs.get_str(WIFI_CONFIG_KEY, &mut config_buffer) {
            Ok(Some(json_str)) => {
                info!("🔍 DEBUG: {} = '{}'", WIFI_CONFIG_KEY, json_str);

                // Try to parse and display structured data
                match serde_json::from_str::<StoredWiFiConfig>(&json_str) {
                    Ok(config) => {
                        info!("🔍 DEBUG:   Parsed Config:");
                        info!("🔍 DEBUG:     SSID: '{}'", config.ssid);
                        info!("🔍 DEBUG:     Password: '{}'", config.password);
                        info!("🔍 DEBUG:     Timestamp: {}", config.timestamp);
                        info!("🔍 DEBUG:     Is Valid: {}", config.is_valid);
                    }
                    Err(e) => {
                        warn!("🔍 DEBUG:   Failed to parse JSON config: {}", e);
                    }
                }
            }
            Ok(None) => {
                info!("🔍 DEBUG: {} = <not found>", WIFI_CONFIG_KEY);
            }
            Err(e) => {
                warn!("🔍 DEBUG: {} = <read error: {:?}>", WIFI_CONFIG_KEY, e);
            }
        }

        // Try to read individual SSID field
        let mut ssid_buffer = [0u8; 64];
        match self.nvs.get_str(SSID_KEY, &mut ssid_buffer) {
            Ok(Some(ssid)) => {
                info!("🔍 DEBUG: {} = '{}'", SSID_KEY, ssid);
            }
            Ok(None) => {
                info!("🔍 DEBUG: {} = <not found>", SSID_KEY);
            }
            Err(e) => {
                warn!("🔍 DEBUG: {} = <read error: {:?}>", SSID_KEY, e);
            }
        }

        // Try to read individual password field
        let mut password_buffer = [0u8; 64];
        match self.nvs.get_str(PASSWORD_KEY, &mut password_buffer) {
            Ok(Some(password)) => {
                info!("🔍 DEBUG: {} = '{}'", PASSWORD_KEY, password);
            }
            Ok(None) => {
                info!("🔍 DEBUG: {} = <not found>", PASSWORD_KEY);
            }
            Err(e) => {
                warn!("🔍 DEBUG: {} = <read error: {:?}>", PASSWORD_KEY, e);
            }
        }

        info!("🔍 DEBUG: End of persistent storage dump");
    }
}
