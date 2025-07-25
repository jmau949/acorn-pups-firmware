// Import ESP-IDF's NVS (Non-Volatile Storage) functionality
// NVS is a key-value storage system that persists data in flash memory
// Data stored in NVS survives device reboots and power cycles
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

// Import anyhow for consistent error handling
use anyhow::{anyhow, Result};

// Import logging macros for debug output
use log::{debug, error, info, warn};

// No longer using JSON serialization - individual keys provide better reliability
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
                                                 // Individual key storage provides reliable persistence without size limitations

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

        debug!("Storing WiFi credentials and auth token to NVS");

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

        match self.nvs.set_str(AUTH_TOKEN_KEY, &credentials.auth_token) {
            Ok(_) => {
                debug!("Auth token stored successfully as string");
            }
            Err(e) => {
                warn!(
                    "Auth token too large for string storage, trying blob: {:?}",
                    e
                );
                // Try storing as blob instead of string
                match self
                    .nvs
                    .set_blob(AUTH_TOKEN_KEY, credentials.auth_token.as_bytes())
                {
                    Ok(_) => {
                        debug!("Auth token stored successfully as blob");
                    }
                    Err(blob_err) => {
                        error!("Failed to store auth token: {:?}", blob_err);
                        return Err(anyhow!("Failed to store auth token: {:?}", blob_err));
                    }
                }
            }
        }

        self.nvs
            .set_str(DEVICE_NAME_KEY, &credentials.device_name)
            .map_err(|e| anyhow!("Failed to store device name: {}", e))?;

        self.nvs
            .set_str(USER_TIMEZONE_KEY, &credentials.user_timezone)
            .map_err(|e| anyhow!("Failed to store user timezone: {}", e))?;

        // Store timestamp separately for metadata tracking
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        match self.nvs.set_u64("timestamp", timestamp) {
            Ok(_) => debug!("WiFi storage timestamp updated"),
            Err(e) => warn!("Failed to store timestamp: {:?}", e),
        }

        info!("WiFi credentials and enhanced data stored successfully");
        Ok(())
    }

    /// Load WiFi credentials from NVS flash memory
    pub fn load_credentials(&mut self) -> Result<Option<WiFiCredentials>> {
        info!("Loading WiFi credentials from NVS");

        // Load from individual keys (primary and only storage method)
        self.load_credentials_from_individual_keys()
    }

    /// Load credentials from individual NVS keys
    fn load_credentials_from_individual_keys(&mut self) -> Result<Option<WiFiCredentials>> {
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
        let mut buffer = [0u8; 2048]; // Much larger buffer for JWT auth tokens (up to 2KB)

        let auth_token = match self.nvs.get_str(AUTH_TOKEN_KEY, &mut buffer) {
            Ok(Some(token)) => {
                debug!("Auth token loaded as string ({} chars)", token.len());
                token.to_string()
            }
            Ok(None) => {
                // Try loading as blob
                let mut blob_buffer = vec![0u8; 2048]; // 2KB buffer for JWT tokens
                match self.nvs.get_blob(AUTH_TOKEN_KEY, &mut blob_buffer) {
                    Ok(Some(blob_data)) => match std::str::from_utf8(blob_data) {
                        Ok(token_str) => {
                            debug!("Auth token loaded as blob ({} chars)", token_str.len());
                            token_str.to_string()
                        }
                        Err(e) => {
                            error!("Auth token blob is not valid UTF-8: {:?}", e);
                            "".to_string()
                        }
                    },
                    Ok(None) => {
                        debug!("No auth token found");
                        "".to_string()
                    }
                    Err(e) => {
                        error!("Failed to load auth token as blob: {:?}", e);
                        "".to_string()
                    }
                }
            }
            Err(e) => {
                error!("Failed to load auth token as string: {:?}", e);
                "".to_string()
            }
        };
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
        let _ = self.nvs.remove("timestamp");

        info!("All WiFi credentials and enhanced data cleared successfully");
        Ok(())
    }

    /// Clear only the auth token (to save space after device registration)
    pub fn clear_auth_token(&mut self) -> Result<()> {
        debug!("Clearing auth token from NVS to save space");
        match self.nvs.remove(AUTH_TOKEN_KEY) {
            Ok(_) => {
                info!("✅ Auth token cleared from storage");
                Ok(())
            }
            Err(e) => {
                warn!("Failed to clear auth token: {:?}", e);
                Ok(()) // Don't fail if already missing
            }
        }
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
                info!("  Password: {}", creds.password);
                info!("  Auth Token: {} characters", creds.auth_token.len());
                info!("  Device Name: {}", creds.device_name);
                info!("  User Timezone: {}", creds.user_timezone);

                // Show timestamp if available
                match self.nvs.get_u64("timestamp") {
                    Ok(Some(timestamp)) => info!("  Stored at: {} (unix timestamp)", timestamp),
                    _ => debug!("  No timestamp available"),
                }
            }
            Ok(None) => info!("  No credentials stored"),
            Err(e) => info!("  Error loading credentials: {}", e),
        }
    }
}
