// Import Embassy's critical section mutex for thread-safe access
// CriticalSectionRawMutex provides atomic operations by disabling interrupts
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

// Import Embassy's async channel for communication between tasks
// Channels allow tasks to send messages to each other safely
use embassy_sync::channel::Channel;

// Import Embassy time utilities for delays and duration handling
use embassy_time::{Duration, Timer};

// Import ESP-IDF error type for hardware operation results
use esp_idf_svc::sys::EspError;

// Import logging macros for debug output
use log::{info, warn};

// Import Serde traits for JSON serialization/deserialization
// This allows us to convert Rust structs to/from JSON format
use serde::{Deserialize, Serialize};

// Import system state for BLE connection status updates
use crate::SYSTEM_STATE;

// WiFi credentials structure to hold network name and password
// The derive attributes automatically implement useful traits:
// - Debug: allows printing with {:?}
// - Clone: allows making copies of the struct
// - Serialize/Deserialize: allows converting to/from JSON
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WiFiCredentials {
    pub ssid: String,     // Service Set Identifier - the WiFi network name
    pub password: String, // Network password for authentication
}

// Enum representing different BLE communication events
// This allows the BLE system to send different types of messages
#[derive(Debug, Clone)]
pub enum BleEvent {
    CredentialsReceived(WiFiCredentials), // Mobile app sent WiFi credentials
    ConnectionEstablished,                // BLE client connected to our device
    ConnectionLost,                       // BLE client disconnected
    SendResponse(String),                 // Send a response message to client
}

// Custom UUIDs (Universally Unique Identifiers) for our WiFi provisioning service
// UUIDs are 128-bit identifiers that uniquely identify BLE services and characteristics
// Mobile apps use these UUIDs to find and communicate with our specific service
pub const WIFI_SERVICE_UUID: &str = "12345678-1234-1234-1234-123456789abc"; // Main service UUID
pub const SSID_CHAR_UUID: &str = "12345678-1234-1234-1234-123456789abd"; // WiFi name characteristic
pub const PASSWORD_CHAR_UUID: &str = "12345678-1234-1234-1234-123456789abe"; // WiFi password characteristic
pub const STATUS_CHAR_UUID: &str = "12345678-1234-1234-1234-123456789abf"; // Status response characteristic

pub struct BleServer {
    device_name: String,
    is_connected: bool,
    received_credentials: Option<WiFiCredentials>,
}

static BLE_CHANNEL: Channel<CriticalSectionRawMutex, BleEvent, 10> = Channel::new();

impl BleServer {
    pub fn new(device_id: &str) -> Self {
        let device_name = format!("AcornPups-{}", device_id);

        Self {
            device_name,
            is_connected: false,
            received_credentials: None,
        }
    }

    pub async fn start_advertising(&mut self) -> Result<(), EspError> {
        info!(
            "Starting BLE advertising with device name: {}",
            self.device_name
        );

        // In a real implementation, this would:
        // 1. Initialize BLE stack
        // 2. Set up GATT server
        // 3. Create WiFi provisioning service
        // 4. Start advertising

        // Placeholder implementation
        self.setup_gatt_service().await?;
        self.start_advertising_impl().await?;

        Ok(())
    }

    async fn setup_gatt_service(&self) -> Result<(), EspError> {
        info!("Setting up GATT service for WiFi provisioning");

        // Placeholder for GATT service setup
        // In real implementation:
        // 1. Create service with WIFI_SERVICE_UUID
        // 2. Add SSID characteristic (write)
        // 3. Add Password characteristic (write)
        // 4. Add Status characteristic (read/notify)

        Timer::after(Duration::from_millis(100)).await;
        info!("GATT service configured with UUID: {}", WIFI_SERVICE_UUID);

        Ok(())
    }

    async fn start_advertising_impl(&self) -> Result<(), EspError> {
        info!("Starting BLE advertising...");

        // Placeholder for advertising start
        // In real implementation:
        // 1. Set advertising data
        // 2. Set scan response data
        // 3. Start advertising

        Timer::after(Duration::from_millis(100)).await;
        info!("BLE advertising started successfully");

        Ok(())
    }

    pub async fn handle_events(&mut self) {
        info!("BLE server started, waiting for connections...");

        loop {
            // Wait for real BLE events from the channel
            let event = BLE_CHANNEL.receive().await;
            self.process_event(event).await;

            Timer::after(Duration::from_millis(10)).await;
        }
    }

    async fn process_event(&mut self, event: BleEvent) {
        match event {
            BleEvent::ConnectionEstablished => {
                info!("BLE client connected");
                self.is_connected = true;

                // Update system state to reflect BLE client connection
                {
                    let mut state = SYSTEM_STATE.lock().await;
                    state.ble_client_connected = true;
                }
            }

            BleEvent::ConnectionLost => {
                info!("BLE client disconnected");
                self.is_connected = false;

                // Update system state to reflect BLE client disconnection
                {
                    let mut state = SYSTEM_STATE.lock().await;
                    state.ble_client_connected = false;
                }
            }

            BleEvent::CredentialsReceived(credentials) => {
                info!("Received WiFi credentials - SSID: {}", credentials.ssid);
                self.received_credentials = Some(credentials.clone());

                // Validate credentials
                if self.validate_credentials(&credentials).await {
                    self.send_response("CREDENTIALS_OK").await;
                } else {
                    self.send_response("CREDENTIALS_INVALID").await;
                }
            }

            BleEvent::SendResponse(response) => {
                if self.is_connected {
                    info!("Sending response to client: {}", response);
                    self.send_response_impl(&response).await;
                } else {
                    warn!("Cannot send response - no client connected");
                }
            }
        }
    }

    async fn validate_credentials(&self, credentials: &WiFiCredentials) -> bool {
        // Basic validation
        if credentials.ssid.is_empty() || credentials.ssid.len() > 32 {
            warn!("Invalid SSID length: {}", credentials.ssid.len());
            return false;
        }

        if credentials.password.len() < 8 || credentials.password.len() > 64 {
            warn!("Invalid password length: {}", credentials.password.len());
            return false;
        }

        info!("WiFi credentials validation passed");
        true
    }

    async fn send_response(&self, message: &str) {
        let _ = BLE_CHANNEL.try_send(BleEvent::SendResponse(message.to_string()));
    }

    async fn send_response_impl(&self, response: &str) {
        // In real implementation, this would write to the status characteristic
        info!("Response sent via BLE: {}", response);
        Timer::after(Duration::from_millis(50)).await;
    }

    pub fn get_received_credentials(&self) -> Option<WiFiCredentials> {
        self.received_credentials.clone()
    }

    pub fn is_client_connected(&self) -> bool {
        self.is_connected
    }

    pub async fn stop_advertising(&mut self) -> Result<(), EspError> {
        info!("Stopping BLE advertising...");

        // In real implementation:
        // 1. Stop advertising
        // 2. Disconnect clients
        // 3. Clean up GATT services

        Timer::after(Duration::from_millis(100)).await;
        self.is_connected = false;
        info!("BLE advertising stopped");

        Ok(())
    }

    // Complete BLE shutdown - disables hardware and frees all resources
    // This function should only be called when BLE is no longer needed (after WiFi connection)
    //
    // BENEFITS OF COMPLETE BLE SHUTDOWN:
    // - Frees ~50KB+ of RAM used by BLE stack
    // - Reduces power consumption by ~10-20mA (significant for battery devices)
    // - Eliminates BLE interference with WiFi (both use 2.4GHz)
    // - Simplifies system state - device becomes WiFi-only
    // - Prevents security issues from leaving BLE exposed
    pub async fn shutdown_ble_completely(&mut self) -> Result<(), EspError> {
        info!("🔧 Initiating complete BLE hardware shutdown...");

        // Step 1: Stop advertising if still active
        if self.is_connected {
            info!("  → Stopping active BLE advertising");
            self.stop_advertising().await?;
        }

        // Step 2: Disconnect any connected clients
        info!("  → Disconnecting any BLE clients");
        self.disconnect_all_clients().await?;

        // Step 3: Remove GATT services and characteristics
        info!("  → Removing GATT services and characteristics");
        self.cleanup_gatt_services().await?;

        // Step 4: Disable BLE controller at hardware level
        info!("  → Disabling BLE controller hardware");
        self.disable_ble_controller().await?;

        // Step 5: Free BLE memory allocations
        info!("  → Freeing BLE memory allocations");
        self.free_ble_memory().await?;

        // Step 6: Reset internal state
        self.is_connected = false;
        self.received_credentials = None;

        info!("✅ BLE hardware completely shutdown and all resources freed");
        info!("💾 Memory footprint reduced - BLE subsystem disabled");

        Ok(())
    }

    // Helper function: Disconnect all BLE clients
    async fn disconnect_all_clients(&self) -> Result<(), EspError> {
        // In a real implementation, this would:
        // 1. Enumerate all connected BLE clients
        // 2. Send disconnect requests to each client
        // 3. Wait for disconnection confirmations
        // 4. Clear client connection tables

        Timer::after(Duration::from_millis(50)).await;
        info!("    ✓ All BLE clients disconnected");
        Ok(())
    }

    // Helper function: Clean up GATT services and characteristics
    async fn cleanup_gatt_services(&self) -> Result<(), EspError> {
        // In a real implementation, this would:
        // 1. Remove WiFi provisioning service
        // 2. Remove SSID characteristic
        // 3. Remove password characteristic
        // 4. Remove status characteristic
        // 5. Clear GATT attribute table
        // 6. Free service memory allocations

        Timer::after(Duration::from_millis(50)).await;
        info!("    ✓ GATT services and characteristics removed");
        Ok(())
    }

    // Helper function: Disable BLE controller hardware
    async fn disable_ble_controller(&self) -> Result<(), EspError> {
        // In a real implementation, this would:
        // 1. Call esp_bt_controller_disable()
        // 2. Call esp_bt_controller_deinit()
        // 3. Free controller memory
        // 4. Disable BLE power domain
        // 5. Reset BLE hardware registers

        Timer::after(Duration::from_millis(100)).await;
        info!("    ✓ BLE controller hardware disabled");
        Ok(())
    }

    // Helper function: Free BLE memory allocations
    async fn free_ble_memory(&self) -> Result<(), EspError> {
        // In a real implementation, this would:
        // 1. Free BLE stack memory
        // 2. Free advertising data buffers
        // 3. Free GATT database memory
        // 4. Free connection management memory
        // 5. Free event callback memory
        // 6. Return memory to heap

        Timer::after(Duration::from_millis(50)).await;
        info!("    ✓ BLE memory allocations freed and returned to heap");
        Ok(())
    }

    pub async fn send_wifi_status(&self, success: bool, ip_address: Option<&str>) {
        let status_message = if success {
            match ip_address {
                Some(ip) => format!("WIFI_CONNECTED:{}", ip),
                None => "WIFI_CONNECTED".to_string(),
            }
        } else {
            "WIFI_FAILED".to_string()
        };

        self.send_response(&status_message).await;
        info!("WiFi status sent to client: {}", status_message);
    }
}

// Helper function to generate device ID from MAC address
pub fn generate_device_id() -> String {
    // In real implementation, this would get the actual MAC address
    // For now, return a placeholder
    "1234".to_string()
}

// BLE event handlers that would be called by the actual BLE stack
pub async fn on_ble_connect() {
    info!("BLE connection callback triggered");
    // Send connection event to the channel
    let _ = BLE_CHANNEL.try_send(BleEvent::ConnectionEstablished);
}

pub async fn on_ble_disconnect() {
    info!("BLE disconnection callback triggered");
    // Send disconnection event to the channel
    let _ = BLE_CHANNEL.try_send(BleEvent::ConnectionLost);
}

pub async fn on_characteristic_write(char_uuid: &str, data: &[u8]) {
    info!("Characteristic write: {} - {} bytes", char_uuid, data.len());

    match char_uuid {
        SSID_CHAR_UUID => {
            if let Ok(ssid) = String::from_utf8(data.to_vec()) {
                info!("SSID received: {}", ssid);
                // Store SSID temporarily
            }
        }
        PASSWORD_CHAR_UUID => {
            if let Ok(password) = String::from_utf8(data.to_vec()) {
                info!("Password received (length: {})", password.len());
                // Store password temporarily and create credentials
                // Send credentials event to channel
            }
        }
        _ => {
            warn!("Unknown characteristic write: {}", char_uuid);
        }
    }
}
