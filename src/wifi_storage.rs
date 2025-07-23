// Import ESP-IDF's NVS (Non-Volatile Storage) functionality
// NVS is a key-value storage system that persists data in flash memory
// Data stored in NVS survives device reboots and power cycles
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

// Import anyhow for consistent error handling
use anyhow::{anyhow, Result};

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
const AUTH_TOKEN_KEY: &str = "auth_token"; // Key for storing authentication token
const DEVICE_NAME_KEY: &str = "device_name"; // Key for storing device name
const USER_TIMEZONE_KEY: &str = "user_timezone"; // Key for storing user timezone
const WIFI_CONFIG_KEY: &str = "wifi_creds"; // Key for storing complete WiFi config as JSON

// Extended WiFi configuration structure for storage
// This includes extra metadata beyond just the basic credentials
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredWiFiConfig {
    pub ssid: String,          // WiFi network name
    pub password: String,      // WiFi password
    pub auth_token: String,    // Authentication token for device registration
    pub device_name: String,   // User-defined device name
    pub user_timezone: String, // User's timezone preference
    pub timestamp: u64,        // When this config was stored (Unix timestamp)
    pub is_valid: bool,        // Whether this config should be used
}

// WiFi storage manager - handles all NVS operations for WiFi credentials
// This struct wraps the ESP-IDF NVS functionality with WiFi-specific logic
pub struct WiFiStorage {
    nvs: EspNvs<NvsDefault>, // ESP-IDF NVS handle for flash storage operations
}

impl WiFiStorage {
    /// Create new WiFi storage instance with default NVS partition
    pub fn new() -> Result<Self> {
        info!("Initializing WiFi storage with default NVS partition");

        // Take the default NVS partition
        let nvs_partition = EspDefaultNvsPartition::take()
            .map_err(|e| anyhow!("Failed to take NVS partition: {}", e))?;

        Self::new_with_partition(nvs_partition)
    }

    /// Create WiFi storage using provided NVS partition
    pub fn new_with_partition(nvs_partition: EspDefaultNvsPartition) -> Result<Self> {
        info!("Initializing WiFi storage with provided NVS partition");

        // Open the NVS namespace for WiFi configuration
        let nvs = EspNvs::new(nvs_partition, NVS_NAMESPACE, true)
            .map_err(|e| anyhow!("Failed to open WiFi NVS namespace: {}", e))?;

        info!("WiFi storage initialized successfully");

        Ok(Self { nvs })
    }

    /// Store WiFi credentials in NVS flash memory
    pub fn store_credentials(&mut self, credentials: &WiFiCredentials) -> Result<()> {
        info!("Storing WiFi credentials for SSID: {}", credentials.ssid);

        // Validate credentials before storing
        if credentials.ssid.is_empty() {
            return Err(anyhow!("SSID cannot be empty"));
        }

        // Store all credential fields
        self.nvs
            .set_str(SSID_KEY, &credentials.ssid)
            .map_err(|e| anyhow!("Failed to store SSID: {}", e))?;
        self.nvs
            .set_str(PASSWORD_KEY, &credentials.password)
            .map_err(|e| anyhow!("Failed to store password: {}", e))?;
        self.nvs
            .set_str(AUTH_TOKEN_KEY, &credentials.auth_token)
            .map_err(|e| anyhow!("Failed to store auth token: {}", e))?;
        self.nvs
            .set_str(DEVICE_NAME_KEY, &credentials.device_name)
            .map_err(|e| anyhow!("Failed to store device name: {}", e))?;
        self.nvs
            .set_str(USER_TIMEZONE_KEY, &credentials.user_timezone)
            .map_err(|e| anyhow!("Failed to store user timezone: {}", e))?;

        // Create extended configuration with metadata
        let config = StoredWiFiConfig {
            ssid: credentials.ssid.clone(),
            password: credentials.password.clone(),
            auth_token: credentials.auth_token.clone(),
            device_name: credentials.device_name.clone(),
            user_timezone: credentials.user_timezone.clone(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            is_valid: true,
        };

        // Store complete configuration as JSON
        let config_json = serde_json::to_string(&config)
            .map_err(|e| anyhow!("Failed to serialize WiFi config: {}", e))?;

        self.nvs
            .set_str(WIFI_CONFIG_KEY, &config_json)
            .map_err(|e| anyhow!("Failed to store WiFi config: {}", e))?;

        info!("WiFi credentials and enhanced data stored successfully");
        Ok(())
    }

    /// Load WiFi credentials from NVS flash memory
    pub fn load_credentials(&mut self) -> Result<Option<WiFiCredentials>> {
        info!("Loading WiFi credentials from NVS");

        // Try to load from complete JSON config first
        if let Ok(Some(config)) = self.load_config() {
            if config.is_valid {
                info!("Loaded WiFi credentials for SSID: {}", config.ssid);
                return Ok(Some(WiFiCredentials {
                    ssid: config.ssid,
                    password: config.password,
                    auth_token: config.auth_token,
                    device_name: config.device_name,
                    user_timezone: config.user_timezone,
                }));
            }
        }

        // Fallback to loading individual keys
        self.load_credentials_fallback()
    }

    /// Internal method to load credentials from individual NVS keys
    fn load_credentials_fallback(&mut self) -> Result<Option<WiFiCredentials>> {
        let mut ssid_buf = [0u8; 32];
        let mut password_buf = [0u8; 64];

        let ssid = match self
            .nvs
            .get_str(SSID_KEY, &mut ssid_buf)
            .map_err(|e| anyhow!("Failed to load SSID: {}", e))?
        {
            Some(ssid) => ssid,
            None => {
                info!("No WiFi credentials found in NVS");
                return Ok(None);
            }
        };

        let password = match self
            .nvs
            .get_str(PASSWORD_KEY, &mut password_buf)
            .map_err(|e| anyhow!("Failed to load password: {}", e))?
        {
            Some(password) => password,
            None => {
                warn!("SSID found but password missing - clearing incomplete credentials");
                let _ = self.clear_credentials();
                return Ok(None);
            }
        };

        info!("Loaded WiFi credentials for SSID: {}", ssid);

        // Load additional fields (with defaults if not present)
        let mut buffer = [0u8; 512]; // Larger buffer for auth tokens
        let auth_token = self
            .nvs
            .get_str(AUTH_TOKEN_KEY, &mut buffer)
            .unwrap_or(None)
            .unwrap_or("")
            .to_string();
        let device_name = self
            .nvs
            .get_str(DEVICE_NAME_KEY, &mut buffer)
            .unwrap_or(None)
            .unwrap_or("")
            .to_string();
        let user_timezone = self
            .nvs
            .get_str(USER_TIMEZONE_KEY, &mut buffer)
            .unwrap_or(None)
            .unwrap_or("")
            .to_string();

        Ok(Some(WiFiCredentials {
            ssid: ssid.to_string(),
            password: password.to_string(),
            auth_token,
            device_name,
            user_timezone,
        }))
    }

    /// Clear all WiFi credentials from NVS
    pub fn clear_credentials(&mut self) -> Result<()> {
        info!("Clearing WiFi credentials from NVS");

        // Remove all credential keys
        let _ = self.nvs.remove(SSID_KEY);
        let _ = self.nvs.remove(PASSWORD_KEY);
        let _ = self.nvs.remove(AUTH_TOKEN_KEY);
        let _ = self.nvs.remove(DEVICE_NAME_KEY);
        let _ = self.nvs.remove(USER_TIMEZONE_KEY);
        let _ = self.nvs.remove(WIFI_CONFIG_KEY);

        info!("All WiFi credentials and enhanced data cleared successfully");
        Ok(())
    }

    /// Load complete WiFi configuration from JSON
    fn load_config(&mut self) -> Result<Option<StoredWiFiConfig>> {
        let mut config_buf = [0u8; 256];

        let config_json = match self
            .nvs
            .get_str(WIFI_CONFIG_KEY, &mut config_buf)
            .map_err(|e| anyhow!("Failed to load WiFi config: {}", e))?
        {
            Some(json) => json,
            None => return Ok(None),
        };

        let config: StoredWiFiConfig = serde_json::from_str(config_json)
            .map_err(|e| anyhow!("Failed to parse WiFi config JSON: {}", e))?;

        Ok(Some(config))
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

    /// Debug utility to display current WiFi storage contents
    pub fn debug_dump_storage(&mut self) {
        info!("🔍 DEBUG: WiFi storage contents:");

        match self.load_credentials() {
            Ok(Some(creds)) => {
                info!("  SSID: {}", creds.ssid);
                info!("  Password: [REDACTED - {} chars]", creds.password.len());
            }
            Ok(None) => info!("  No credentials stored"),
            Err(e) => info!("  Error loading credentials: {}", e),
        }

        match self.load_config() {
            Ok(Some(config)) => {
                info!("  Config timestamp: {}", config.timestamp);
                info!("  Config valid: {}", config.is_valid);
            }
            Ok(None) => info!("  No config found"),
            Err(e) => info!("  Error loading config: {}", e),
        }
    }
}
