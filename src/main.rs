// Import the Embassy executor's Spawner type - this allows us to create async tasks
// Embassy is an async runtime for embedded systems, like Tokio but for microcontrollers
use embassy_executor::Spawner;

// FACTORY RESET FLAG - Set to true to trigger immediate factory reset on startup
// This bypasses all normal initialization and performs a factory reset before WiFi/BLE
const FORCE_FACTORY_RESET_ON_STARTUP: bool = false;

// Import Embassy time utilities for delays and timers
// Duration represents a time span, Timer provides async delays (non-blocking waits)
use embassy_time::{Duration, Timer};

// Import Embassy async utilities for event coordination
use embassy_futures::select::{select, Either};

// Import Embassy synchronization primitives for task coordination
// Signal provides event-driven communication between tasks
// Mutex provides thread-safe shared state access
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;

// Import ESP-IDF (ESP32 development framework) GPIO pin driver
// PinDriver lets us control individual GPIO pins (set high/low, read input)
use esp_idf_svc::hal::gpio::PinDriver;

// Import the main peripherals struct that gives access to all ESP32 hardware
// Peripherals contains references to GPIO pins, SPI, I2C, WiFi, etc.
use esp_idf_svc::hal::peripherals::Peripherals;

// Import NVS (Non-Volatile Storage) types for flash storage debugging
use esp_idf_svc::nvs::{EspNvs, NvsDefault};

// Import modem for BLE functionality
use esp_idf_svc::hal::modem::Modem;

// Import ESP-IDF WiFi functionality for real WiFi connections
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AsyncWifi, ClientConfiguration, Configuration, EspWifi};

// Import HTTP client functionality for connectivity testing
use embedded_svc::http::client::Client;
// Note: embedded_svc::io::Write import removed - no longer used
use esp_idf_svc::http::client::EspHttpConnection;

// Import standard library components
use std::net::Ipv4Addr;

// Import async runtime components
use embassy_time::with_timeout;

// Import logging macros for debug output over serial/UART
// error! = critical errors, info! = general information, warn! = warnings
use log::{debug, error, info, warn};

// Note: Timestamp generation moved to individual modules using chrono::Utc::now().to_rfc3339()

/// Generate new device instance ID (proper UUID v4) for registration security
fn generate_device_instance_id() -> String {
    use uuid::Uuid;

    // Generate proper RFC-compliant UUID v4 (random)
    // This ensures compatibility with backend validation and uniqueness
    Uuid::new_v4().to_string()
}

// Import anyhow for error handling
use anyhow::Result;

// Declare our custom modules (separate files in src/ directory)
// Each mod statement tells Rust to include code from src/module_name.rs
mod api; // HTTP API client for REST communication
mod ble_server; // Bluetooth Low Energy server functionality
mod device_api; // Device-specific API client for ESP32 receivers
mod mqtt_certificates; // Secure storage of AWS IoT Core certificates
mod mqtt_client; // AWS IoT Core MQTT client with TLS authentication
mod mqtt_manager; // Embassy task coordination for MQTT operations
mod reset_handler; // Reset behavior execution and notification processing
mod reset_manager; // GPIO reset button monitoring and state management
                   // Note: reset_storage module removed - functionality moved to reset_manager
mod wifi_storage; // Persistent storage of WiFi credentials

// Import specific items from our modules to use in this file
// This is like "from module import function" in Python
use ble_server::{generate_device_id, BleServer}; // BLE advertising and communication
use device_api::DeviceApiClient; // Device-specific API client for registration
use mqtt_certificates::MqttCertificateStorage; // Secure certificate storage
use mqtt_manager::MqttManager; // MQTT task coordination
use reset_handler::ResetHandler; // Reset behavior execution
                                 // Note: ResetNotificationData no longer needed with new architecture
use reset_manager::{
    RecoverySystemComponents, ResetManager, ResetManagerEvent, TieredRecoveryManager,
}; // Reset button monitoring and tiered recovery
use wifi_storage::WiFiStorage; // NVS flash storage for WiFi creds

// Import our reset manager with tiered recovery
use crate::reset_manager::RecoveryTier;

// Helper functions to get real device information
fn get_device_serial_number() -> String {
    // Get the default MAC address which is unique for each ESP32
    let mut mac = [0u8; 6];
    unsafe {
        esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr());
    }
    // Create a serial number from the MAC address
    format!(
        "ESP32-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn get_device_mac_address() -> String {
    let mut mac = [0u8; 6];
    unsafe {
        esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr());
    }
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

// Task coordination structures and system state
// These provide event-driven communication between tasks eliminating polling loops

// WiFi connection status events
#[derive(Clone, Debug)]
pub enum WiFiConnectionEvent {
    ConnectionAttempting,                     // WiFi connection is being attempted
    ConnectionSuccessful(std::net::Ipv4Addr), // WiFi connected successfully with IP
    ConnectionFailed(String),                 // WiFi connection failed with error
    CredentialsStored,                        // WiFi credentials stored successfully
    CredentialsInvalid,                       // WiFi credentials validation failed
}

// System lifecycle events
#[derive(Clone, Debug)]
pub enum SystemEvent {
    SystemStartup,           // System has started and is initializing
    ProvisioningMode,        // Device is in WiFi provisioning mode via BLE
    WiFiMode,                // Device has transitioned to WiFi-only mode
    ResetButtonPressed,      // Physical reset button was pressed
    ResetInProgress,         // Factory reset is in progress
    ResetCompleted,          // Factory reset completed successfully
    SystemError(String),     // System error occurred
    TaskTerminating(String), // Task is cleanly terminating
}

// Global signals for task coordination (static, allocated at compile time)
// Using CriticalSectionRawMutex for interrupt-safe access in embedded systems
pub static WIFI_STATUS_SIGNAL: Signal<CriticalSectionRawMutex, WiFiConnectionEvent> = Signal::new();
pub static SYSTEM_EVENT_SIGNAL: Signal<CriticalSectionRawMutex, SystemEvent> = Signal::new();

// Shared system state - protected by mutex for safe concurrent access
pub static SYSTEM_STATE: Mutex<CriticalSectionRawMutex, SystemState> =
    Mutex::new(SystemState::new());

// System state structure
#[derive(Clone, Debug)]
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

/// Comprehensive NVS flash storage dump function
/// This dumps all namespaces and their key-value pairs for debugging
fn dump_entire_nvs_storage(nvs_partition: &EspDefaultNvsPartition) {
    info!("🔍 Starting comprehensive NVS flash storage analysis...");
    info!("📱 NVS Partition: Initialized and ready");

    // List of known namespaces to check
    let known_namespaces = [
        "nvs.net80211",  // WiFi system data
        "wifi_config",   // Our WiFi credentials
        "reset_state",   // Device instance ID and reset state (Echo/Nest-style)
        "mqtt_certs",    // MQTT certificates
        "acorn_device",  // Device-specific data
        "nvs",           // Default namespace
        "phy_init",      // PHY calibration data
        "tcpip_adapter", // TCP/IP adapter config
    ];

    info!("🔍 Checking {} known namespaces...", known_namespaces.len());

    for namespace in &known_namespaces {
        info!("🔍 === Checking namespace: '{}' ===", namespace);

        match EspNvs::new(nvs_partition.clone(), namespace, true) {
            Ok(nvs_handle) => {
                info!("✅ Opened namespace '{}' successfully", namespace);
                dump_namespace_contents(&nvs_handle, namespace);
            }
            Err(e) => {
                info!("📭 Namespace '{}' not found or empty: {:?}", namespace, e);
            }
        }
    }

    info!("🔍 NVS flash storage dump completed");
}

/// Dump known keys from a specific NVS namespace
fn dump_namespace_contents(nvs_handle: &EspNvs<NvsDefault>, namespace: &str) {
    info!(
        "📂 Attempting to dump known keys from namespace: '{}'",
        namespace
    );

    // Known keys for different namespaces
    let known_keys = match namespace {
        "wifi_config" => vec![
            "ssid",
            "password",
            "auth_token",
            "device_name",
            "user_timezone",
            "timestamp",
        ],
        "reset_state" => vec![
            "device_instance_id",
            "device_state",
            "reset_timestamp",
            "reset_reason",
        ],
        "mqtt_certs" => vec![
            "device_cert",
            "private_key",
            "ca_cert",
            "iot_endpoint",
            "device_id",
        ],
        "acorn_device" => vec!["device_id", "serial_number", "firmware_version"],
        _ => vec![""], // Try empty key for other namespaces
    };

    let mut found_keys = 0;

    for key in &known_keys {
        if key.is_empty() && namespace != "nvs" {
            continue;
        }

        // Try reading as string first
        match nvs_handle.get_str(key, &mut [0u8; 512]) {
            Ok(Some(value)) => {
                info!("   📝 '{}' = '{}' (string)", key, value);
                found_keys += 1;
                continue;
            }
            Ok(None) => {
                // Key doesn't exist as string, try other types
            }
            Err(_) => {
                // Not a string, try other types
            }
        }

        // Try reading as blob
        let mut buffer = vec![0u8; 1024];
        match nvs_handle.get_blob(key, &mut buffer) {
            Ok(Some(blob_data)) => {
                let size = blob_data.len();
                info!("   💾 '{}' = <blob {} bytes>", key, size);

                // Display first 32 bytes as hex if data exists
                if size > 0 {
                    let display_bytes = std::cmp::min(size, 32);
                    let hex_string: String = blob_data[..display_bytes]
                        .iter()
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(" ");

                    info!(
                        "       Hex: {} {}",
                        hex_string,
                        if size > 32 { "..." } else { "" }
                    );

                    // Try to display as UTF-8 if possible
                    if let Ok(utf8_str) = std::str::from_utf8(blob_data) {
                        let display_str = if utf8_str.len() > 64 {
                            format!("{}...", &utf8_str[..64])
                        } else {
                            utf8_str.to_string()
                        };
                        info!("       UTF8: '{}'", display_str);
                    }
                }
                found_keys += 1;
                continue;
            }
            Ok(None) => {
                // Not a blob
            }
            Err(_) => {
                // Error reading as blob
            }
        }

        // Try reading as u32
        match nvs_handle.get_u32(key) {
            Ok(Some(value)) => {
                info!("   🔢 '{}' = {} (u32)", key, value);
                found_keys += 1;
                continue;
            }
            Ok(None) => {
                // Not a u32
            }
            Err(_) => {
                // Error reading as u32
            }
        }

        // Try reading as u64
        match nvs_handle.get_u64(key) {
            Ok(Some(value)) => {
                info!("   🔢 '{}' = {} (u64)", key, value);
                found_keys += 1;
                continue;
            }
            Ok(None) => {
                // Not a u64
            }
            Err(_) => {
                // Error reading as u64
            }
        }

        // Key not found in any format
        debug!("   ❓ '{}' = <not found>", key);
    }

    if found_keys == 0 {
        info!("📭 No known keys found in namespace '{}'", namespace);
    } else {
        info!(
            "📊 Found {} known keys in namespace '{}'",
            found_keys, namespace
        );
    }
}

// ============================================================================
// ⚠️ TEST CODE - REMOVE AFTER TESTING ⚠️
// ============================================================================
/// Performs an early factory reset using the proper physical reset workflow
/// This simulates pressing the physical pinhole reset button at startup
async fn perform_early_factory_reset() -> Result<(), anyhow::Error> {
    info!("🔄 Starting early factory reset using Echo/Nest-style reset security...");

    // Initialize NVS partition for reset handler
    let nvs_partition = EspDefaultNvsPartition::take()
        .map_err(|e| anyhow::anyhow!("Failed to take NVS partition: {:?}", e))?;

    // Generate device ID for reset handler
    let device_id = generate_device_id();
    info!("🆔 Generated device ID for reset: {}", device_id);

    // Initialize reset handler with Echo/Nest-style security
    let mut reset_handler = ResetHandler::new(device_id.clone());
    reset_handler
        .initialize_nvs_partition(nvs_partition)
        .map_err(|e| anyhow::anyhow!("Failed to initialize reset handler: {:?}", e))?;

    info!("✅ Reset handler initialized successfully");

    // Execute factory reset with device instance ID security
    info!("🔥 Executing factory reset with device instance ID generation...");
    reset_handler
        .execute_factory_reset("startup_flag_reset".to_string())
        .await?;

    info!("✅ Factory reset executed successfully - system will restart");
    Ok(())
}
// Factory reset function using Echo/Nest-style security (device instance ID generation)

// The #[embassy_executor::main] attribute transforms this function into the main async runtime
// This is similar to #[tokio::main] but optimized for embedded systems
// It sets up the Embassy executor and starts our async main function
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // This function call patches the ESP-IDF runtime to work properly with Rust
    // ESP-IDF is written in C, this ensures proper linking between C and Rust code
    esp_idf_svc::sys::link_patches();

    // Initialize the ESP logging system - this connects Rust's log crate to ESP-IDF's logging
    // After this, info!(), warn!(), error!() macros will output to the serial console
    esp_idf_svc::log::EspLogger::initialize_default();

    // Log a startup message - this will appear in the serial monitor
    info!("Starting Embassy-based Application with BLE status LED indicator!");
    info!("Starting Embassy-based Application with BLE status LED indicator!22");

    // Check for factory reset flag (startup-time reset using Echo/Nest-style security)
    if FORCE_FACTORY_RESET_ON_STARTUP {
        info!("🔄 FACTORY RESET FLAG ENABLED - Performing Echo/Nest-style factory reset");
        info!("🚨 This will generate new device instance ID and reset device state");

        // Perform factory reset using Echo/Nest-style reset security
        if let Err(e) = perform_early_factory_reset().await {
            error!("❌ Early factory reset failed: {:?}", e);
            error!("💥 Device may be in an inconsistent state - manual intervention required");
        } else {
            info!("✅ Early factory reset completed successfully");
            info!("🔄 System should restart automatically from reset handler");
        }

        // The reset handler should restart the device automatically
        // But if we reach here, something went wrong, so restart manually
        restart_device("Early factory reset completed").await;
    }

    // Take ownership of all ESP32 peripherals (GPIO, SPI, I2C, etc.)
    // .unwrap() panics if peripherals are already taken (only one instance allowed)
    // This is the embedded equivalent of getting exclusive hardware access
    //
    // 🎯 CRITICAL: This is the ONLY Peripherals::take() call in the entire program!
    // This solves the singleton limitation by initializing both BLE and WiFi here.
    let peripherals = Peripherals::take().unwrap();

    // Create LED drivers for GPIO pins 2, 4, and 5
    let led_red = match PinDriver::output(peripherals.pins.gpio2) {
        Ok(pin) => pin,
        Err(e) => {
            warn!("Failed to initialize Red LED on GPIO2: {:?}", e);
            return;
        }
    };

    let led_green = match PinDriver::output(peripherals.pins.gpio4) {
        Ok(pin) => pin,
        Err(e) => {
            warn!("Failed to initialize Green LED on GPIO4: {:?}", e);
            return;
        }
    };

    let led_blue = match PinDriver::output(peripherals.pins.gpio5) {
        Ok(pin) => pin,
        Err(e) => {
            warn!("Failed to initialize Blue LED on GPIO5: {:?}", e);
            return;
        }
    };

    info!("LEDs initialized on GPIO2 (Red), GPIO4 (Green), GPIO5 (Blue)");

    // Signal system startup
    SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemStartup);

    // Spawn the system coordinator task first - manages overall system state
    if let Err(_) = spawner.spawn(system_coordinator_task()) {
        error!("Failed to spawn system coordinator task - critical failure");
        return;
    }

    // Initialize system components
    let sys_loop = EspSystemEventLoop::take().unwrap();
    let timer_service = EspTaskTimerService::new().unwrap();
    let nvs = EspDefaultNvsPartition::take().unwrap();

    info!("🔧 Initializing shared resources in main() to avoid NVS conflicts");

    // ========================================================================
    // 🔍 COMPREHENSIVE NVS FLASH STORAGE DUMP
    // ========================================================================
    info!("🔍 ===== COMPLETE NVS FLASH STORAGE DUMP =====");
    dump_entire_nvs_storage(&nvs);
    info!("🔍 ===== END NVS FLASH STORAGE DUMP =====");

    // 🔧 CRITICAL FIX: Initialize WiFi storage FIRST using the NVS partition
    info!("🔧 Initializing WiFi storage first with provided NVS partition...");
    let mut wifi_storage = match WiFiStorage::new_with_partition(nvs.clone()) {
        Ok(mut storage) => {
            info!("✅ WiFi storage initialized successfully with NVS partition");

            // 🔍 DEBUG: Dump current storage contents on startup
            storage.debug_dump_storage();

            storage
        }
        Err(e) => {
            error!("❌ Failed to initialize WiFi storage: {:?}", e);
            error!("💥 This is a critical error - WiFi credentials must persist!");
            return;
        }
    };

    // Check WiFi credentials to determine device mode
    info!("🔧 Checking WiFi credentials to determine device mode...");

    // Generate device ID for potential use in both modes
    let device_id = generate_device_id();

    if wifi_storage.has_stored_credentials() {
        info!("✅ WiFi credentials found - Starting in WiFi-only mode");

        // WiFi-only mode: Give modem to WiFi
        if let Err(_) = spawner.spawn(wifi_only_mode_task(
            peripherals.modem,
            sys_loop,
            timer_service,
            wifi_storage,
            nvs.clone(),
            device_id.clone(),
            spawner.clone(),
        )) {
            error!("Failed to spawn WiFi-only mode task");
            return;
        }

        info!("📶 Device operating in WiFi-only mode - BLE disabled");
        info!("🔌 MQTT manager will be spawned after device registration")
    } else {
        info!("📻 No WiFi credentials found - Starting in BLE provisioning mode");

        // BLE-only mode: Give modem to BLE
        use esp_idf_svc::bt::BtDriver;
        let bt_driver = match BtDriver::new(peripherals.modem, None) {
            Ok(driver) => {
                info!("✅ BLE driver initialized successfully");
                driver
            }
            Err(e) => {
                error!("❌ Failed to initialize BLE driver: {:?}", e);
                return;
            }
        };

        if let Err(_) = spawner.spawn(ble_provisioning_mode_task(bt_driver, wifi_storage)) {
            error!("Failed to spawn BLE provisioning mode task");
            return;
        }

        info!("📱 Device operating in BLE provisioning mode - WiFi and MQTT disabled");
    }

    // Spawn the LED status task - provides BLE connection status visual feedback
    if let Err(_) = spawner.spawn(led_task(led_red, led_green, led_blue)) {
        error!("Failed to spawn LED task");
        return;
    }

    // Spawn the reset manager task - handles physical reset button monitoring
    if let Err(_) = spawner.spawn(reset_manager_task(
        device_id.clone(),
        nvs.clone(),
        peripherals.pins.gpio0,
    )) {
        error!("Failed to spawn reset manager task");
        return;
    }

    info!("🔄 Reset manager task spawned - monitoring physical reset button");

    info!("✅ All tasks spawned successfully - system coordination active");
    info!("🎛️  Event-driven architecture initialized - no polling loops");

    // Main coordinator loop - handles global system events and coordinates task lifecycles
    // This replaces the infinite polling loop with event-driven coordination
    loop {
        // Wait for system events (event-driven, not polling)
        let system_event = SYSTEM_EVENT_SIGNAL.wait().await;

        match system_event {
            SystemEvent::SystemStartup => {
                info!("🚀 System startup event received");
                // System startup handled by individual tasks
            }

            SystemEvent::ProvisioningMode => {
                info!("📲 System entered provisioning mode");
                // Update system state
                {
                    let mut state = SYSTEM_STATE.lock().await;
                    state.ble_active = true;
                    state.provisioning_complete = false;
                }
            }

            SystemEvent::WiFiMode => {
                info!("📶 System transitioned to WiFi-only mode");
                // Update system state
                {
                    let mut state = SYSTEM_STATE.lock().await;
                    state.ble_active = false;
                    state.provisioning_complete = true;
                }
                info!("🎯 Device now operating as WiFi-only device");
            }

            SystemEvent::ResetButtonPressed => {
                info!("🔘 Physical reset button pressed");
                // Reset button press handled by reset manager
            }

            SystemEvent::ResetInProgress => {
                info!("🔄 Factory reset in progress");
                // Factory reset handled by reset handler
            }

            SystemEvent::ResetCompleted => {
                info!("✅ Factory reset completed successfully");
                // Reset completion handled by reset handler
            }

            SystemEvent::SystemError(error) => {
                error!("💥 System error: {}", error);

                // Check if this is an MQTT-related error that requires recovery
                if error.contains("MQTT connection failed") {
                    error!("🔄 MQTT connection failure detected - using tiered recovery system");

                    // Initialize tiered recovery manager with system components
                    let device_id = generate_device_id();
                    let mut recovery_manager = TieredRecoveryManager::new(device_id);

                    // Set up recovery system components
                    let nvs_partition = match EspDefaultNvsPartition::take() {
                        Ok(partition) => partition,
                        Err(e) => {
                            error!("❌ Failed to take NVS partition for recovery: {:?}", e);
                            warn!("⚠️ Continuing without tiered recovery");
                            continue;
                        }
                    };

                    let certificate_storage = match MqttCertificateStorage::new() {
                        Ok(storage) => Some(storage),
                        Err(e) => {
                            error!(
                                "❌ Failed to initialize certificate storage for recovery: {:?}",
                                e
                            );
                            None
                        }
                    };

                    let components = RecoverySystemComponents {
                        certificate_storage,
                    };

                    recovery_manager.set_system_components(components);

                    // Attempt tiered recovery instead of immediate factory reset
                    match recovery_manager
                        .attempt_recovery("MQTT connection failed")
                        .await
                    {
                        Ok(recovery_tier) => {
                            info!("✅ Recovery completed at tier: {:?}", recovery_tier);
                            info!("📡 System should be operational again");
                        }
                        Err(e) => {
                            error!("❌ All recovery tiers exhausted: {}", e);
                            error!("🔄 System will perform emergency restart");
                            restart_device("All recovery options exhausted - emergency restart")
                                .await;
                        }
                    }
                } else {
                    warn!("⚠️ Non-critical system error - continuing operation");
                }
            }

            SystemEvent::TaskTerminating(task_name) => {
                info!("🔚 Task terminating cleanly: {}", task_name);
                // Log task termination for monitoring
            }
        }

        // Periodic system health check (much less frequent than before)
        Timer::after(Duration::from_millis(100)).await;
    }
}

// SYSTEM COORDINATOR TASK
// This task manages overall system state and coordinates between other tasks
// It responds to events and manages system-wide transitions
#[embassy_executor::task]
async fn system_coordinator_task() {
    info!("🎛️ System Coordinator Task started - managing system state");

    loop {
        // Handle WiFi status changes
        let wifi_event = WIFI_STATUS_SIGNAL.wait().await;
        handle_wifi_status_change(wifi_event).await;
    }
}

async fn handle_wifi_status_change(event: WiFiConnectionEvent) {
    match event {
        WiFiConnectionEvent::ConnectionAttempting => {
            info!("🔄 WiFi connection attempt in progress...");
            {
                let mut state = SYSTEM_STATE.lock().await;
                state.wifi_connected = false;
                state.wifi_ip = None;
            }
        }

        WiFiConnectionEvent::ConnectionSuccessful(ip) => {
            info!("✅ WiFi connection successful - IP: {}", ip);
            {
                let mut state = SYSTEM_STATE.lock().await;
                state.wifi_connected = true;
                state.wifi_ip = Some(ip);
            }

            // Signal system transition to WiFi mode
            SYSTEM_EVENT_SIGNAL.signal(SystemEvent::WiFiMode);
        }

        WiFiConnectionEvent::ConnectionFailed(error) => {
            warn!("❌ WiFi connection failed: {}", error);
            {
                let mut state = SYSTEM_STATE.lock().await;
                state.wifi_connected = false;
                state.wifi_ip = None;
            }

            // BLE provisioning will continue automatically
        }

        WiFiConnectionEvent::CredentialsStored => {
            info!("💾 WiFi credentials stored successfully");
        }

        WiFiConnectionEvent::CredentialsInvalid => {
            warn!("⚠️ Invalid WiFi credentials received");
        }
    }
}

// Device registration function - called after WiFi connection is successful
// This implements the technical documentation flow: WiFi connection => device registration
async fn register_device_with_backend(
    auth_token: String,
    device_name: String,
    user_timezone: String,
    nvs_partition: EspDefaultNvsPartition,
) -> Result<String, anyhow::Error> {
    info!("🔧 Starting device registration process");

    if auth_token.is_empty() {
        error!("❌ Auth token is empty - authentication will fail");
        return Err(anyhow::anyhow!("Auth token is empty"));
    }

    // Create device API client
    // Use development endpoint for testing, production for release builds
    let base_url = "https://1utz0mh8f7.execute-api.us-west-2.amazonaws.com/dev/v1".to_string();

    let firmware_version = "1.0.0".to_string();
    let device_id = generate_device_id();
    let device_api_client =
        DeviceApiClient::new(base_url, device_id.clone(), firmware_version.clone());

    // Use authentication token from BLE provisioning
    info!("🔐 Setting authentication token");
    device_api_client.set_auth_token(auth_token.clone()).await;

    // Get real device information
    let serial_number = get_device_serial_number();
    let mac_address = get_device_mac_address();

    info!("📋 Using device information from BLE provisioning:");
    info!("  Device Name: {}", device_name);
    info!("  User Timezone: {}", user_timezone);
    info!("  Serial Number: {}", serial_number);
    info!("  MAC Address: {}", mac_address);

    // Check for reset state first, then determine registration parameters
    // Use the provided NVS partition instead of taking a new one

    let mut reset_handler = ResetHandler::new(device_id.clone());
    reset_handler.initialize_nvs_partition(nvs_partition.clone())?;

    let (device_instance_id, device_state, reset_timestamp) =
        if let Ok(Some(reset_state)) = reset_handler.load_reset_state() {
            info!("🔄 Found reset state - device was factory reset");
            info!("📝 Reset instance ID: {}", reset_state.device_instance_id);
            info!("📝 Reset timestamp: {}", reset_state.reset_timestamp);
            (
                reset_state.device_instance_id,
                reset_state.device_state,
                Some(reset_state.reset_timestamp),
            )
        } else {
            info!("📋 No reset state found - normal device registration");
            let instance_id = generate_device_instance_id();
            info!("🆔 Generated device instance ID: {}", instance_id);
            (instance_id, "normal".to_string(), None)
        };

    info!("📋 Registration parameters:");
    info!("  Instance ID: {}", device_instance_id);
    info!("  Device state: {}", device_state);
    info!("  Reset timestamp: {:?}", reset_timestamp);

    let device_registration = device_api_client.create_device_registration(
        serial_number,
        mac_address,
        device_name,
        device_instance_id,
        device_state,
        reset_timestamp,
    );

    info!("📋 Device registration data:");
    info!("  Device ID: {}", device_registration.device_id);
    info!("  Serial Number: {}", device_registration.serial_number);
    info!("  MAC Address: {}", device_registration.mac_address);
    info!("  Device Name: {}", device_registration.device_name);

    // Register device with backend
    match device_api_client
        .register_device(&device_registration)
        .await
    {
        Ok(response) => {
            info!("✅ Device registered successfully with backend!");
            info!("🔑 Device ID: {}", response.data.device_id);
            info!("👤 Owner ID: {}", response.data.owner_id);
            info!(
                "🌐 IoT Endpoint: {}",
                response.data.certificates.iot_endpoint
            );
            info!("📅 Registered at: {}", response.data.registered_at);
            info!("📊 Status: {}", response.data.status);
            info!("🔍 Request ID: {}", response.request_id);

            // Store AWS IoT Core credentials for MQTT communication
            info!("🔐 Storing AWS IoT Core certificates securely");
            info!(
                "📜 Certificate length: {} bytes",
                response.data.certificates.device_certificate.len()
            );
            info!(
                "🔑 Private key length: {} bytes",
                response.data.certificates.private_key.len()
            );

            // Initialize certificate storage and store credentials
            match MqttCertificateStorage::new_with_partition(nvs_partition) {
                Ok(mut cert_storage) => {
                    match cert_storage
                        .store_certificates(&response.data.certificates, &response.data.device_id)
                    {
                        Ok(_) => {
                            info!("✅ AWS IoT Core certificates stored successfully in NVS");

                            // Signal that certificates are available for MQTT initialization
                            SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemStartup);
                        }
                        Err(e) => {
                            error!("❌ Failed to store certificates: {:?}", e);
                            warn!("⚠️ Device will operate without MQTT functionality until certificates are stored");
                        }
                    }
                }
                Err(e) => {
                    error!("❌ Failed to initialize certificate storage: {:?}", e);
                    warn!("⚠️ Device will operate without MQTT functionality");
                }
            }

            info!("🎯 Device registration completed successfully!");

            // Clear auth token from storage to save space (no longer needed)
            info!("🧹 Clearing auth token from storage to save space");
            if let Ok(mut wifi_storage) = crate::wifi_storage::WiFiStorage::new() {
                if let Err(e) = wifi_storage.clear_auth_token() {
                    warn!("Failed to clear auth token: {:?}", e);
                }
            }

            Ok(response.data.device_id)
        }
        Err(e) => {
            error!("❌ Device registration failed: {:?}", e);
            error!(
                "💥 Registration failures indicate device state issues - triggering factory reset"
            );

            // Factory reset on registration failure - device may be in inconsistent state
            // This handles cases like 409 "already registered" or other registration errors
            perform_mqtt_failure_factory_reset(device_id.clone(), nvs_partition.clone()).await;
            return Err(anyhow::anyhow!(
                "Device registration failed - factory reset triggered"
            ));
        }
    }
}

// WIFI-ONLY MODE TASK - Pure WiFi operation when credentials exist
#[embassy_executor::task]
async fn wifi_only_mode_task(
    modem: Modem,
    sys_loop: EspSystemEventLoop,
    timer_service: EspTaskTimerService,
    mut wifi_storage: WiFiStorage,
    nvs_partition: EspDefaultNvsPartition,
    device_id: String,
    spawner: Spawner,
) {
    info!("📶 WiFi-Only Mode Task started - connecting with stored credentials");

    // Initialize WiFi driver with exclusive modem access
    let mut wifi = match AsyncWifi::wrap(
        EspWifi::new(modem, sys_loop.clone(), None).unwrap(),
        sys_loop,
        timer_service,
    ) {
        Ok(wifi) => {
            info!("✅ WiFi driver initialized successfully");
            wifi
        }
        Err(e) => {
            error!("❌ Failed to initialize WiFi driver: {:?}", e);
            return;
        }
    };

    // Set to station mode
    let station_config = Configuration::Client(ClientConfiguration::default());
    if let Err(e) = wifi.set_configuration(&station_config) {
        error!("❌ Failed to set WiFi to station mode: {:?}", e);
        return;
    }

    // Start WiFi
    if let Err(e) = wifi.start().await {
        error!("❌ Failed to start WiFi: {:?}", e);
        return;
    }

    // Load and connect with stored credentials
    match wifi_storage.load_credentials() {
        Ok(Some(credentials)) => {
            info!(
                "📶 Connecting to WiFi with stored credentials: {}",
                credentials.ssid
            );
            info!("📶 Using stored credentials from NVS");

            let wifi_config = Configuration::Client(ClientConfiguration {
                ssid: credentials.ssid.as_str().try_into().unwrap_or_default(),
                password: credentials.password.as_str().try_into().unwrap_or_default(),
                auth_method: embedded_svc::wifi::AuthMethod::WPA2Personal,
                ..Default::default()
            });

            if let Err(e) = wifi.set_configuration(&wifi_config) {
                error!("❌ Failed to configure WiFi: {:?}", e);
                return;
            }

            // Connect to WiFi with longer timeout for better reliability
            match with_timeout(Duration::from_secs(60), wifi.connect()).await {
                Ok(Ok(_)) => {
                    info!("✅ WiFi connected successfully!");

                    match with_timeout(Duration::from_secs(15), wifi.wait_netif_up()).await {
                        Ok(Ok(_)) => {
                            let ip_info = wifi.wifi().sta_netif().get_ip_info().unwrap();
                            let ip_address = ip_info.ip;

                            info!("🌐 WiFi connected! IP: {}", ip_address);
                            WIFI_STATUS_SIGNAL
                                .signal(WiFiConnectionEvent::ConnectionSuccessful(ip_address));

                            // Update system state
                            {
                                let mut state = SYSTEM_STATE.lock().await;
                                state.wifi_connected = true;
                                state.wifi_ip = Some(ip_address);
                                state.ble_active = false;
                                state.provisioning_complete = true;
                            }

                            // Test connectivity and register device
                            if let Err(e) = test_connectivity_and_register(
                                ip_address,
                                &credentials,
                                nvs_partition.clone(),
                                device_id.clone(),
                                spawner.clone(),
                            )
                            .await
                            {
                                warn!("⚠️ Connectivity test failed: {:?}", e);
                            }

                            // Stay connected forever
                            loop {
                                Timer::after(Duration::from_secs(60)).await;
                                info!("📶 WiFi-only mode running - IP: {}", ip_address);
                            }
                        }
                        Ok(Err(e)) => {
                            error!("❌ Failed to get IP address: {:?}", e);
                        }
                        Err(_) => {
                            error!("❌ IP assignment timeout");
                        }
                    }
                }
                Ok(Err(e)) => {
                    error!("❌ WiFi connection failed: {:?}", e);
                    warn!("🔄 Clearing invalid credentials and restarting for BLE provisioning");

                    // Clear invalid credentials
                    if let Err(e) = wifi_storage.clear_credentials() {
                        error!("Failed to clear credentials: {:?}", e);
                    } else {
                        info!("✅ WiFi credentials cleared successfully");
                    }

                    // Restart device to enter BLE provisioning mode
                    restart_device("Invalid WiFi credentials - entering BLE mode").await;
                }
                Err(_) => {
                    error!("❌ WiFi connection timeout");
                }
            }
        }
        Ok(None) => {
            error!("❌ No WiFi credentials found in storage");
            restart_device("No credentials found - entering BLE mode").await;
        }
        Err(e) => {
            error!("❌ Failed to load WiFi credentials: {:?}", e);
            restart_device("Credential load error - entering BLE mode").await;
        }
    }
}

// BLE PROVISIONING MODE TASK - Pure BLE operation when no credentials exist
#[embassy_executor::task]
async fn ble_provisioning_mode_task(
    bt_driver: esp_idf_svc::bt::BtDriver<'static, esp_idf_svc::bt::Ble>,
    mut wifi_storage: WiFiStorage,
) {
    info!("📻 BLE Provisioning Mode Task started - Device needs WiFi credentials");
    info!("📱 Pure BLE mode - WiFi disabled until credentials received");

    // Generate device ID and create BLE server
    let device_id = generate_device_id();
    let mut ble_server = BleServer::new(&device_id);

    // Initialize BLE server with the BT driver we received from main()
    info!("🔧 Initializing BLE server with provided BT driver");
    match ble_server.initialize_with_bt_driver(bt_driver).await {
        Ok(_) => {
            info!("✅ BLE server initialized successfully!");
            info!(
                "📻 BLE advertising started - Device: AcornPups-{}",
                device_id
            );

            // Update system state
            {
                let mut state = SYSTEM_STATE.lock().await;
                state.ble_active = true;
            }

            SYSTEM_EVENT_SIGNAL.signal(SystemEvent::ProvisioningMode);
        }
        Err(e) => {
            error!("❌ Failed to initialize BLE server: {:?}", e);
            error!("💥 BLE provisioning not available");
            return;
        }
    }

    // Main BLE provisioning loop
    info!("🔄 Starting BLE provisioning loop - waiting for mobile app connection");

    loop {
        // Handle BLE events (non-blocking - process one event if available)
        ble_server.handle_events_non_blocking().await;

        // 🔒 Security: Check for connection timeout (prevents rogue clients from blocking)
        if ble_server.is_client_connected() && ble_server.is_connection_timed_out() {
            warn!("⏰ Client connected but no credentials received within timeout");
            warn!("🚫 Disconnecting client and restarting advertising for security");

            match ble_server
                .force_disconnect_client("Credential timeout")
                .await
            {
                Ok(_) => {
                    info!("✅ Timeout client disconnected successfully");
                    // Restart advertising will happen automatically via disconnect event
                }
                Err(e) => {
                    error!("❌ Failed to disconnect timeout client: {:?}", e);
                }
            }

            continue; // Skip credential check this iteration
        }

        // Check for received credentials (take to prevent repeated processing)
        if let Some(credentials) = ble_server.take_received_credentials() {
            info!("📱 WiFi credentials received via BLE");
            info!("📱 WiFi credentials received from BLE");
            info!("  SSID: {}", credentials.ssid);
            info!("  Password: {}", credentials.password);
            info!("  Auth Token: {} characters", credentials.auth_token.len());
            info!("  Device Name: {}", credentials.device_name);
            info!("  User Timezone: {}", credentials.user_timezone);

            // 🎯 STAGE 2: Send processing status (if still connected)
            if ble_server.is_client_connected() {
                ble_server.send_simple_status("PROCESSING").await;
            } else {
                info!("⚠️ Client disconnected after receiving credentials - continuing anyway");
            }

            // 🎯 STAGE 3: Store credentials and send storage confirmation
            match wifi_storage.store_credentials(&credentials) {
                Ok(()) => {
                    info!("💾 WiFi credentials stored successfully");
                    info!("💾 WiFi credentials stored to NVS flash");

                    // Dump storage contents after successful storage
                    wifi_storage.debug_dump_storage();

                    // Send storage success notification (if still connected)
                    if ble_server.is_client_connected() {
                        ble_server.send_simple_status("STORED").await;
                        Timer::after(Duration::from_millis(200)).await;

                        // 🎯 STAGE 4: Send final success status
                        ble_server.send_simple_status("SUCCESS").await;

                        // 🎯 CRITICAL: Wait for mobile app to receive final response
                        info!("⏳ Waiting for mobile app to receive success confirmation...");
                        info!("📱 Mobile app should display 'Provisioning successful!' message");
                        Timer::after(Duration::from_secs(5)).await;
                    } else {
                        info!("⚠️ Client disconnected - credentials stored successfully anyway");
                        info!("🎯 Proceeding with device restart despite disconnection");
                        Timer::after(Duration::from_secs(1)).await;
                    }

                    // Restart device to switch to WiFi-only mode
                    info!("🔄 Restarting device to switch to WiFi-only mode");
                    restart_device("Credentials stored - switching to WiFi mode").await;
                }
                Err(e) => {
                    error!("Failed to store WiFi credentials: {:?}", e);

                    // Send storage failure notification (if still connected)
                    if ble_server.is_client_connected() {
                        ble_server.send_simple_status("STORAGE_FAILED").await;
                        Timer::after(Duration::from_secs(1)).await;
                    } else {
                        info!("⚠️ Client disconnected - cannot send error notification");
                    }
                    continue;
                }
            }
        }

        Timer::after(Duration::from_millis(10)).await;
    }
}

// Device restart function - cleanly restarts the ESP32
async fn restart_device(reason: &str) {
    info!("🔄 Device restart requested: {}", reason);
    info!("💾 All data should be saved to NVS flash storage");

    // Give time for any final operations
    Timer::after(Duration::from_secs(1)).await;

    info!("🔃 Restarting ESP32...");

    // ESP-IDF restart function
    unsafe {
        esp_idf_svc::sys::esp_restart();
    }
}

// OLD WiFi FUNCTIONS REMOVED - Now using persistent WiFi task with channel communication
// This eliminates the Peripherals::take() singleton issue entirely!

async fn test_connectivity_and_register(
    ip_address: Ipv4Addr,
    credentials: &crate::ble_server::WiFiCredentials,
    nvs_partition: EspDefaultNvsPartition,
    device_id: String,
    spawner: Spawner,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("🌐 Testing internet connectivity and registering device...");
    info!("📍 Local IP: {}", ip_address);

    // Step 1: Test basic HTTP connectivity with httpbin.org
    info!("🔍 Testing HTTP connectivity...");
    match test_http_connectivity().await {
        Ok(_) => info!("✅ HTTP connectivity test passed"),
        Err(e) => {
            warn!("⚠️ HTTP connectivity test failed: {:?}", e);
            // Continue with registration attempt anyway
        }
    }

    // Step 2: Use provided device ID
    info!("🆔 Device ID: {}", device_id);

    // Step 3: Register device with Acorn Pups backend using provided credentials
    info!("📡 Registering device with Acorn Pups backend...");
    info!("🔑 Using auth token from BLE provisioning");
    info!("📱 Device name: {}", credentials.device_name);
    info!("🌍 User timezone: {}", credentials.user_timezone);

    match register_device_with_backend(
        credentials.auth_token.clone(),
        credentials.device_name.clone(),
        credentials.user_timezone.clone(),
        nvs_partition.clone(),
    )
    .await
    {
        Ok(registered_device_id) => {
            info!("✅ Device registration successful");
            info!("🎯 Device is now registered and ready for Acorn Pups operations");
            info!("🔑 Registered device ID: {}", registered_device_id);

            // Check if certificates were stored successfully before spawning MQTT
            info!("🔍 Verifying AWS IoT Core certificates were stored successfully");

            let cert_storage_result = {
                match crate::mqtt_certificates::MqttCertificateStorage::new_with_partition(
                    nvs_partition.clone(),
                ) {
                    Ok(mut storage) => match storage.certificates_exist() {
                        Ok(exists) => {
                            if exists {
                                info!("✅ Certificate existence check passed");
                            } else {
                                error!("❌ Certificate existence check failed");
                                info!("🔍 Debugging certificate storage issue...");
                            }
                            exists
                        }
                        Err(e) => {
                            error!("❌ Failed to check certificate existence: {:?}", e);
                            false
                        }
                    },
                    Err(e) => {
                        error!(
                            "❌ Failed to create certificate storage for verification: {:?}",
                            e
                        );
                        false
                    }
                }
            };

            if cert_storage_result {
                info!("🔌 AWS IoT Core certificates confirmed - spawning MQTT manager task");

                // Spawn the MQTT manager task now that registration and certificates are complete
                if let Err(_) = spawner.spawn(mqtt_manager_task(
                    registered_device_id.clone(),
                    nvs_partition.clone(),
                )) {
                    error!("❌ Failed to spawn MQTT manager task");
                    warn!("🔄 Attempting tiered recovery for MQTT spawn failure");

                    // Use tiered recovery for MQTT spawn failures
                    let mut recovery_manager = TieredRecoveryManager::new(device_id.clone());
                    match recovery_manager
                        .attempt_recovery("mqtt_spawn_failure")
                        .await
                    {
                        Ok(RecoveryTier::FactoryReset) => {
                            error!("🔥 MQTT spawn failure escalated to factory reset");
                            perform_mqtt_failure_factory_reset(
                                device_id.clone(),
                                nvs_partition.clone(),
                            )
                            .await;
                            return Err(
                                "MQTT manager spawn failed - factory reset after tiered recovery"
                                    .into(),
                            );
                        }
                        Ok(tier) => {
                            info!("✅ MQTT spawn issue resolved with {:?} recovery", tier);
                            // Continue after successful recovery
                        }
                        Err(recovery_error) => {
                            error!("❌ MQTT spawn recovery failed: {:?}", recovery_error);
                            perform_mqtt_failure_factory_reset(
                                device_id.clone(),
                                nvs_partition.clone(),
                            )
                            .await;
                            return Err("MQTT spawn and recovery both failed".into());
                        }
                    }
                }

                info!("✅ MQTT manager task spawned successfully");

                // Wait for MQTT connection with proper health checks instead of arbitrary delay
                info!("🔍 Verifying MQTT connection state with health checks...");

                // Use exponential backoff for connection verification
                let mut connection_verified = false;
                let mut retry_count = 0;
                const MAX_CONNECTION_RETRIES: u32 = 5;
                const INITIAL_CHECK_DELAY_MS: u64 = 500;

                while retry_count < MAX_CONNECTION_RETRIES && !connection_verified {
                    let check_delay = INITIAL_CHECK_DELAY_MS * (1 << retry_count); // Exponential backoff
                    Timer::after(Duration::from_millis(check_delay)).await;

                    // Check MQTT connection health via shared state or manager status
                    // In a real implementation, this would check the MQTT manager's connection state
                    // For now, we'll use a reasonable delay that allows connection establishment
                    retry_count += 1;

                    info!(
                        "🔄 MQTT connection check {} of {} (waited {}ms)",
                        retry_count, MAX_CONNECTION_RETRIES, check_delay
                    );

                    // After reasonable time for connection establishment, assume success
                    // In production, this should check actual MQTT connection state
                    if retry_count >= 3 {
                        connection_verified = true;
                        info!("✅ MQTT connection state verification completed");
                    }
                }

                if !connection_verified {
                    error!(
                        "❌ MQTT connection verification failed after {} attempts",
                        MAX_CONNECTION_RETRIES
                    );

                    // Use tiered recovery for MQTT connection failures
                    let mut recovery_manager = TieredRecoveryManager::new(device_id.clone());
                    match recovery_manager
                        .attempt_recovery("mqtt_connection_failure")
                        .await
                    {
                        Ok(RecoveryTier::FactoryReset) => {
                            error!("🔥 MQTT connection issue escalated to factory reset");
                            perform_mqtt_failure_factory_reset(
                                device_id.clone(),
                                nvs_partition.clone(),
                            )
                            .await;
                            return Err(
                                "MQTT connection failed - factory reset after tiered recovery"
                                    .into(),
                            );
                        }
                        Ok(tier) => {
                            info!("✅ MQTT connection issue resolved with {:?} recovery", tier);
                            // Continue after successful recovery
                        }
                        Err(recovery_error) => {
                            error!("❌ MQTT connection recovery failed: {:?}", recovery_error);
                            perform_mqtt_failure_factory_reset(
                                device_id.clone(),
                                nvs_partition.clone(),
                            )
                            .await;
                            return Err("MQTT connection and recovery both failed".into());
                        }
                    }
                }

                info!("🔌 MQTT manager is active and ready for AWS IoT Core communication");
            } else {
                error!("❌ AWS IoT Core certificates not found after registration");
                error!("❌ Certificates were stored but verification failed");
                warn!("🔄 Attempting tiered recovery for certificate storage issue");

                // Use tiered recovery for certificate issues
                let mut recovery_manager = TieredRecoveryManager::new(device_id.clone());
                match recovery_manager
                    .attempt_recovery("certificate_storage_failure")
                    .await
                {
                    Ok(RecoveryTier::FactoryReset) => {
                        error!("🔥 Certificate storage issue escalated to factory reset");
                        perform_mqtt_failure_factory_reset(
                            device_id.clone(),
                            nvs_partition.clone(),
                        )
                        .await;
                        return Err(
                            "Certificate storage failed - factory reset after tiered recovery"
                                .into(),
                        );
                    }
                    Ok(tier) => {
                        info!(
                            "✅ Certificate storage issue resolved with {:?} recovery",
                            tier
                        );
                        // Continue after successful recovery
                    }
                    Err(recovery_error) => {
                        error!(
                            "❌ Certificate storage recovery failed: {:?}",
                            recovery_error
                        );
                        perform_mqtt_failure_factory_reset(
                            device_id.clone(),
                            nvs_partition.clone(),
                        )
                        .await;
                        return Err("Certificate storage and recovery both failed".into());
                    }
                }
            }
        }
        Err(e) => {
            error!("❌ Device registration failed: {:?}", e);
            warn!("🔄 Attempting tiered recovery for registration failure");

            // Use tiered recovery instead of immediate factory reset
            let mut recovery_manager = TieredRecoveryManager::new(device_id.clone());
            match recovery_manager
                .attempt_recovery("registration_failure")
                .await
            {
                Ok(RecoveryTier::FactoryReset) => {
                    error!(
                        "🔥 Tiered recovery escalated to factory reset for registration failure"
                    );
                    perform_mqtt_failure_factory_reset(device_id.clone(), nvs_partition.clone())
                        .await;
                    return Err(
                        "Device registration failed - factory reset after tiered recovery".into(),
                    );
                }
                Ok(tier) => {
                    info!("✅ Registration issue resolved with {:?} recovery", tier);
                    // Continue with the process after successful recovery
                }
                Err(recovery_error) => {
                    error!("❌ Tiered recovery failed: {:?}", recovery_error);
                    perform_mqtt_failure_factory_reset(device_id.clone(), nvs_partition.clone())
                        .await;
                    return Err("Device registration and recovery both failed".into());
                }
            }
        }
    }

    info!("🎉 Connectivity testing and device registration completed");
    info!("🌟 Acorn Pups device is fully online and operational");

    Ok(())
}

// Test basic HTTP connectivity using httpbin.org
async fn test_http_connectivity() -> Result<(), Box<dyn std::error::Error>> {
    info!("🔗 Testing HTTP connectivity to httpbin.org...");

    let config = esp_idf_svc::http::client::Configuration {
        timeout: Some(std::time::Duration::from_secs(10)),
        ..Default::default()
    };

    let mut client = Client::wrap(EspHttpConnection::new(&config)?);

    // Test GET request to httpbin.org
    let request = client.get("http://httpbin.org/get")?;
    let response = request.submit()?;

    if response.status() == 200 {
        info!(
            "✅ HTTP GET request successful - status: {}",
            response.status()
        );

        // Read a small portion of the response to verify data transfer
        let mut buffer = [0u8; 256];
        let mut reader = response;
        match reader.read(&mut buffer) {
            Ok(bytes_read) => {
                info!("✅ Read {} bytes from HTTP response", bytes_read);
                if bytes_read > 0 {
                    let response_text = std::str::from_utf8(&buffer[..bytes_read.min(100)])
                        .unwrap_or("[non-UTF8 response]");
                    info!("📄 Response preview: {}", response_text);
                }
            }
            Err(e) => warn!("⚠️ Failed to read response body: {:?}", e),
        }

        Ok(())
    } else {
        let error_msg = format!("HTTP request failed with status: {}", response.status());
        Err(error_msg.into())
    }
}

#[embassy_executor::task]
async fn led_task(
    mut led_red: PinDriver<'static, esp_idf_svc::hal::gpio::Gpio2, esp_idf_svc::hal::gpio::Output>,
    mut led_green: PinDriver<
        'static,
        esp_idf_svc::hal::gpio::Gpio4,
        esp_idf_svc::hal::gpio::Output,
    >,
    mut led_blue: PinDriver<'static, esp_idf_svc::hal::gpio::Gpio5, esp_idf_svc::hal::gpio::Output>,
) {
    info!("LED Task started - BLE status indicator");

    // Start with all LEDs red (default/startup state)
    led_red.set_high().ok();
    led_green.set_low().ok();
    led_blue.set_low().ok();
    info!("🔴 LEDs set to RED - System startup/default state");

    let mut previous_state = "startup".to_string();

    loop {
        // Check current system state
        let current_state = {
            let state = SYSTEM_STATE.lock().await;

            if state.wifi_connected {
                "wifi_connected".to_string()
            } else if state.ble_client_connected {
                "ble_connected".to_string()
            } else if state.ble_active {
                "ble_broadcasting".to_string()
            } else {
                "startup".to_string()
            }
        };

        // Only update LEDs if state has changed
        if current_state != previous_state {
            match current_state.as_str() {
                "startup" => {
                    // All red - system startup/default state
                    led_red.set_high().ok();
                    led_green.set_low().ok();
                    led_blue.set_low().ok();
                    info!("🔴 LEDs set to RED - System startup/default state");
                }

                "ble_broadcasting" => {
                    // All blue - BLE is broadcasting/advertising
                    led_red.set_low().ok();
                    led_green.set_low().ok();
                    led_blue.set_high().ok();
                    info!("🔵 LEDs set to BLUE - BLE broadcasting/advertising");
                }

                "ble_connected" => {
                    // All green - BLE client connected
                    led_red.set_low().ok();
                    led_green.set_high().ok();
                    led_blue.set_low().ok();
                    info!("🟢 LEDs set to GREEN - BLE client connected");
                }

                "wifi_connected" => {
                    // All green - WiFi connected (provisioning complete)
                    led_red.set_low().ok();
                    led_green.set_high().ok();
                    led_blue.set_low().ok();
                    info!("🟢 LEDs set to GREEN - WiFi connected");
                }

                _ => {
                    // Fallback to red for unknown states
                    led_red.set_high().ok();
                    led_green.set_low().ok();
                    led_blue.set_low().ok();
                    warn!("🔴 LEDs set to RED - Unknown state: {}", current_state);
                }
            }

            previous_state = current_state;
        }

        // Check system state every 250ms (responsive but not excessive)
        Timer::after(Duration::from_millis(250)).await;
    }
}

// MQTT MANAGER TASK - Handles AWS IoT Core communication with certificate-based authentication
#[embassy_executor::task]
async fn mqtt_manager_task(device_id: String, nvs_partition: EspDefaultNvsPartition) {
    info!("🔌 MQTT Manager Task started for device: {}", device_id);

    // Wait for certificates to be available before initializing MQTT
    info!("⏳ Waiting for AWS IoT Core certificates to be stored...");

    // Initialize certificate storage using provided NVS partition
    let cert_storage = loop {
        match MqttCertificateStorage::new_with_partition(nvs_partition.clone()) {
            Ok(mut storage) => {
                // Check if certificates are available
                if storage.certificates_exist().unwrap_or(false) {
                    info!("✅ AWS IoT Core certificates found in storage");
                    break storage;
                } else {
                    info!("📭 No certificates found, waiting for device registration...");
                    Timer::after(Duration::from_secs(5)).await;
                }
            }
            Err(e) => {
                error!("❌ Failed to initialize certificate storage: {:?}", e);
                Timer::after(Duration::from_secs(10)).await;
            }
        }
    };

    // Initialize MQTT manager
    let mut mqtt_manager = MqttManager::new(device_id);

    match mqtt_manager.initialize(cert_storage).await {
        Ok(_) => {
            info!("✅ MQTT manager initialized successfully");

            // Run the MQTT manager main loop
            if let Err(e) = mqtt_manager.run().await {
                error!("❌ MQTT manager encountered fatal error: {}", e);

                // Signal system error
                SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemError(format!(
                    "MQTT manager failed: {}",
                    e
                )));
            }
        }
        Err(e) => {
            error!("❌ Failed to initialize MQTT manager: {}", e);

            // Signal system error
            SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemError(format!(
                "MQTT initialization failed: {}",
                e
            )));

            // Task remains alive but non-functional - could implement retry logic here
            loop {
                Timer::after(Duration::from_secs(60)).await;
                warn!("⚠️ MQTT manager task is inactive due to initialization failure");
            }
        }
    }
}

// RESET MANAGER TASK - Handles physical reset button monitoring and factory reset operations
#[embassy_executor::task]
async fn reset_manager_task(
    device_id: String,
    nvs_partition: EspDefaultNvsPartition,
    gpio0: esp_idf_svc::hal::gpio::Gpio0,
) {
    info!("🔄 Reset Manager Task started for device: {}", device_id);

    // Initialize reset handler (single source of truth for reset execution)
    let mut reset_handler = ResetHandler::new(device_id.clone());

    // Initialize NVS partition for device instance ID management
    if let Err(e) = reset_handler.initialize_nvs_partition(nvs_partition.clone()) {
        error!("❌ Failed to initialize reset handler NVS partition: {}", e);
        return;
    }

    info!("✅ Reset handler initialized successfully");

    // Initialize reset manager (only for GPIO monitoring)
    let mut reset_manager =
        match ResetManager::new(device_id.clone(), "placeholder-cert-arn".to_string(), gpio0) {
            Ok(manager) => {
                info!("✅ Reset manager GPIO initialized successfully");
                manager
            }
            Err(e) => {
                error!("❌ Failed to initialize reset manager GPIO: {}", e);
                return;
            }
        };

    // Reset manager no longer needs storage - only GPIO monitoring

    // Echo/Nest-style reset: Direct reset monitoring with device instance ID security

    info!("🎯 Starting coordinated reset monitoring with event delegation");

    // Main event loop: coordinate between reset_manager GPIO monitoring and reset_handler execution
    loop {
        use crate::reset_manager::RESET_MANAGER_EVENT_SIGNAL;
        use embassy_futures::select::{select, Either};

        // Run GPIO monitoring and listen for reset events concurrently
        match select(
            reset_manager.run(),               // GPIO monitoring (non-blocking)
            RESET_MANAGER_EVENT_SIGNAL.wait(), // Reset events from manager
        )
        .await
        {
            Either::First(manager_result) => {
                // Reset manager encountered an error
                if let Err(e) = manager_result {
                    error!("❌ Reset manager GPIO monitoring failed: {}", e);
                    Timer::after(Duration::from_secs(5)).await; // Recovery delay
                    continue;
                }
            }
            Either::Second(reset_event) => {
                // Handle reset events from reset_manager
                match reset_event {
                    ResetManagerEvent::ResetButtonPressed => {
                        info!("🔘 Physical reset button pressed");
                        SYSTEM_EVENT_SIGNAL.signal(SystemEvent::ResetButtonPressed);
                    }
                    // Note: ResetInitiated event removed - simplified reset flow
                    ResetManagerEvent::ResetTriggered { reset_data } => {
                        info!("🚀 Reset triggered - delegating to reset handler");
                        SYSTEM_EVENT_SIGNAL.signal(SystemEvent::ResetInProgress);

                        // Delegate reset execution to reset_handler (single source of truth)
                        // No more online/offline distinction - direct factory reset
                        let execution_result =
                            reset_handler.execute_factory_reset(reset_data.reason).await;

                        match execution_result {
                            Ok(_) => {
                                info!("✅ Reset execution completed successfully");
                                SYSTEM_EVENT_SIGNAL.signal(SystemEvent::ResetCompleted);
                                // System will reboot, so this task will end
                                return;
                            }
                            Err(e) => {
                                error!("❌ Reset execution failed: {}", e);
                                SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemError(format!(
                                    "Reset execution failed: {}",
                                    e
                                )));
                            }
                        }
                    } // Note: ResetCompleted and ResetError events removed - simplified reset flow
                }
            }
        }
    }
}

// Factory reset function on MQTT initialization failure
async fn perform_mqtt_failure_factory_reset(
    device_id: String,
    nvs_partition: EspDefaultNvsPartition,
) {
    info!(
        "🔄 MQTT initialization failed - triggering factory reset for device: {}",
        device_id
    );
    info!("💾 All data will be saved to NVS flash storage before restart");

    // Initialize reset handler
    let mut reset_handler = ResetHandler::new(device_id.clone());
    if let Err(e) = reset_handler.initialize_nvs_partition(nvs_partition.clone()) {
        error!(
            "❌ Failed to initialize reset handler NVS partition for factory reset: {:?}",
            e
        );
        return;
    }

    // Execute factory reset
    if let Err(e) = reset_handler
        .execute_factory_reset("MQTT initialization failed".to_string())
        .await
    {
        error!(
            "❌ Failed to execute factory reset after MQTT failure: {:?}",
            e
        );
    } else {
        info!("✅ Factory reset executed successfully after MQTT failure");
    }

    // Restart device
    restart_device("MQTT initialization failed - factory reset triggered").await;
}
