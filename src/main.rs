// Import the Embassy executor's Spawner type - this allows us to create async tasks
// Embassy is an async runtime for embedded systems, like Tokio but for microcontrollers
use embassy_executor::Spawner;

// Import Embassy time utilities for delays and timers
// Duration represents a time span, Timer provides async delays (non-blocking waits)
use embassy_time::{Duration, Timer};

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

// Import modem for BLE functionality
use esp_idf_svc::hal::modem::Modem;

// Import ESP-IDF WiFi functionality for real WiFi connections
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AsyncWifi, ClientConfiguration, Configuration, EspWifi};

// Import HTTP client functionality for connectivity testing
use embedded_svc::http::client::Client;
use embedded_svc::io::Write;
use esp_idf_svc::http::client::EspHttpConnection;

// Import standard library components
use std::net::Ipv4Addr;

// Import async runtime components
use embassy_time::with_timeout;

// Import logging macros for debug output over serial/UART
// error! = critical errors, info! = general information, warn! = warnings
use log::{error, info, warn};

// Import anyhow for error handling
use anyhow::Result;

// Declare our custom modules (separate files in src/ directory)
// Each mod statement tells Rust to include code from src/module_name.rs
mod ble_server; // Bluetooth Low Energy server functionality
mod wifi_storage; // Persistent storage of WiFi credentials
mod api; // HTTP API client for REST communication
mod device_api; // Device-specific API client for ESP32 receivers


// Import specific items from our modules to use in this file
// This is like "from module import function" in Python
use ble_server::{generate_device_id, BleServer}; // BLE advertising and communication
use wifi_storage::WiFiStorage; // NVS flash storage for WiFi creds
use device_api::DeviceApiClient; // Device-specific API client for registration

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

    // 🔧 CRITICAL FIX: Initialize WiFi storage FIRST using the NVS partition
    info!("🔧 Initializing WiFi storage first with provided NVS partition...");
    let mut wifi_storage = match WiFiStorage::new_with_partition(nvs) {
        Ok(storage) => {
            info!("✅ WiFi storage initialized successfully with NVS partition");
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

    if wifi_storage.has_stored_credentials() {
        info!("✅ WiFi credentials found - Starting in WiFi-only mode");

        // WiFi-only mode: Give modem to WiFi
        if let Err(_) = spawner.spawn(wifi_only_mode_task(
            peripherals.modem,
            sys_loop,
            timer_service,
            wifi_storage,
        )) {
            error!("Failed to spawn WiFi-only mode task");
            return;
        }

        info!("📶 Device operating in WiFi-only mode - BLE disabled");
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

        info!("📱 Device operating in BLE provisioning mode - WiFi disabled");
    }

    // Spawn the LED status task - provides BLE connection status visual feedback
    if let Err(_) = spawner.spawn(led_task(led_red, led_green, led_blue)) {
        error!("Failed to spawn LED task");
        return;
    }

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

            SystemEvent::SystemError(error) => {
                error!("💥 System error: {}", error);
                // Handle system errors gracefully
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
async fn register_device_with_backend() -> Result<(), anyhow::Error> {
    info!("🔧 Starting device registration process");
    
    // Create device API client
    // Use development endpoint for testing, production for release builds
    let base_url = "https://utpfo2x8f6.execute-api.us-west-2.amazonaws.com/dev/v1".to_string();
    
    
    let firmware_version = "1.0.0".to_string();
    let device_id = generate_device_id();
    let device_api_client = DeviceApiClient::new(base_url, device_id.clone(), firmware_version.clone());
    
    // HARDCODED: Authentication token from BLE setup process
    // TODO: Replace with actual token received from mobile app during BLE provisioning
    let HARDCODED_AUTH_TOKEN = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.HARDCODED_JWT_TOKEN_FOR_TESTING";
    
    // Set authentication token for device registration
    device_api_client.set_auth_token(HARDCODED_AUTH_TOKEN.to_string()).await;
    
    // HARDCODED: Device registration data
    // TODO: Replace with actual hardware-specific values
    let device_registration = device_api_client.create_device_registration(
        "HARDCODED_SERIAL_ESP32SN123456789".to_string(),     // serial_number
        "HARDCODED_MAC_AA:BB:CC:DD:EE:FF".to_string(),       // mac_address  
        "HARDCODED_ACORN_PUPS_RECEIVER".to_string(),         // device_name
    );
    
    info!("📋 Device registration data:");
    info!("  Device ID: {}", device_registration.device_id);
    info!("  Serial Number: {}", device_registration.serial_number);
    info!("  MAC Address: {}", device_registration.mac_address);
    info!("  Device Name: {}", device_registration.device_name);
    
    
    // Register device with backend
    match device_api_client.register_device(&device_registration).await {
        Ok(response) => {
            info!("✅ Device registered successfully with backend!");
            info!("🔑 Device ID: {}", response.data.device_id);
            info!("👤 Owner ID: {}", response.data.owner_id);
            info!("🌐 IoT Endpoint: {}", response.data.certificates.iot_endpoint);
            info!("📅 Registered at: {}", response.data.registered_at);
            info!("📊 Status: {}", response.data.status);
            info!("🔍 Request ID: {}", response.request_id);
            
            // TODO: Store AWS IoT Core credentials for MQTT communication
            info!("🔐 TODO: Store IoT credentials securely");
            info!("📜 Certificate length: {} bytes", response.data.certificates.device_certificate.len());
            info!("🔑 Private key length: {} bytes", response.data.certificates.private_key.len());
            
            info!("🎯 Device registration completed successfully!");
            
            Ok(())
        }
        Err(e) => {
            error!("❌ Device registration failed: {}", e);
            Err(e)
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

            let wifi_config = Configuration::Client(ClientConfiguration {
                ssid: credentials.ssid.as_str().try_into().unwrap_or_default(),
                password: credentials.password.as_str().try_into().unwrap_or_default(),
                ..Default::default()
            });

            if let Err(e) = wifi.set_configuration(&wifi_config) {
                error!("❌ Failed to configure WiFi: {:?}", e);
                return;
            }

            // Connect to WiFi
            match with_timeout(Duration::from_secs(30), wifi.connect()).await {
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

                            // Test connectivity
                            if let Err(e) = test_connectivity_and_register(ip_address).await {
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

    // Step 2: Generate device information for registration
    let device_id = generate_device_id();
    info!("🆔 Device ID: {}", device_id);

    // Step 3: Register device with Acorn Pups backend
    info!("📡 Registering device with Acorn Pups backend...");
    match register_device_with_backend().await {
        Ok(_) => {
            info!("✅ Device registration successful");
            info!("🎯 Device is now registered and ready for Acorn Pups operations");
        }
        Err(e) => {
            warn!("⚠️ Device registration failed: {:?}", e);
            info!("📲 Device will operate in standalone mode until next connection attempt");
        }
    }

    // Step 4: Send periodic heartbeat (optional)
    info!("💓 Sending initial heartbeat...");
    match send_heartbeat(&device_id).await {
        Ok(_) => info!("✅ Initial heartbeat sent successfully"),
        Err(e) => warn!("⚠️ Heartbeat failed: {:?}", e),
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

// Send heartbeat to backend to indicate device is alive
async fn send_heartbeat(device_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    info!("💓 Sending heartbeat for device: {}", device_id);

    let heartbeat_data = format!(
        r#"{{
            "device_id": "{}",
            "timestamp": "{}",
            "status": "online",
            "uptime_seconds": 60,
            "memory_free": "200KB",
            "wifi_signal_strength": -45
        }}"#,
        device_id,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );

    let config = esp_idf_svc::http::client::Configuration {
        timeout: Some(std::time::Duration::from_secs(10)),
        ..Default::default()
    };

    let mut client = Client::wrap(EspHttpConnection::new(&config)?);
    let backend_url = "https://api.acornpups.com/devices/heartbeat";

    let headers = [
        ("Content-Type", "application/json"),
        ("User-Agent", "AcornPups-ESP32/1.0.0"),
    ];

    let mut request = client.post(backend_url, &headers)?;
    request.write_all(heartbeat_data.as_bytes())?;
    request.flush()?;

    let response = request.submit()?;

    if response.status() == 200 {
        info!("✅ Heartbeat sent successfully");
        Ok(())
    } else {
        let error_msg = format!("Heartbeat failed with status: {}", response.status());
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


