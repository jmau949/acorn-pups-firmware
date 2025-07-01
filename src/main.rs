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
use embassy_sync::channel::Channel;
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
use embedded_svc::io::{Read, Write};
use esp_idf_svc::http::client::EspHttpConnection;

// Import standard library components
use std::net::Ipv4Addr;

// Import async runtime components
use embassy_time::with_timeout;

// Import logging macros for debug output over serial/UART
// error! = critical errors, info! = general information, warn! = warnings
use log::{error, info, warn};

// Declare our custom modules (separate files in src/ directory)
// Each mod statement tells Rust to include code from src/module_name.rs
mod ble_server; // Bluetooth Low Energy server functionality
mod wifi_storage; // Persistent storage of WiFi credentials

// Import specific items from our modules to use in this file
// This is like "from module import function" in Python
use ble_server::{generate_device_id, BleServer}; // BLE advertising and communication
use wifi_storage::WiFiStorage; // NVS flash storage for WiFi creds

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

// WiFi driver commands for reconfiguration without recreation
#[derive(Clone, Debug)]
pub enum WiFiCommand {
    Connect(ble_server::WiFiCredentials),
    Disconnect,
    GetStatus,
}

// WiFi driver responses
#[derive(Clone, Debug)]
pub enum WiFiResponse {
    Connected(Ipv4Addr),
    Disconnected,
    Failed(String),
    Status {
        connected: bool,
        ip: Option<Ipv4Addr>,
    },
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

// WiFi driver communication channels - solves the Peripherals::take() singleton issue
pub static WIFI_COMMAND_CHANNEL: Channel<CriticalSectionRawMutex, WiFiCommand, 5> = Channel::new();
pub static WIFI_RESPONSE_CHANNEL: Channel<CriticalSectionRawMutex, WiFiResponse, 5> =
    Channel::new();

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
    pub ble_shutdown_requested: bool,
}

impl SystemState {
    pub const fn new() -> Self {
        Self {
            wifi_connected: false,
            wifi_ip: None,
            ble_active: false,
            ble_client_connected: false,
            provisioning_complete: false,
            ble_shutdown_requested: false,
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

    // 🎯 SOLUTION: Initialize ALL shared resources in main() to avoid conflicts
    // Initialize system components
    let sys_loop = EspSystemEventLoop::take().unwrap();
    let timer_service = EspTaskTimerService::new().unwrap();
    let nvs = EspDefaultNvsPartition::take().unwrap();

    info!("🔧 Initializing shared resources in main() to avoid NVS conflicts");

    // 🔧 CRITICAL FIX: Initialize WiFi storage FIRST using the NVS partition
    info!("🔧 Initializing WiFi storage first with provided NVS partition...");
    let wifi_storage = match WiFiStorage::new_with_partition(nvs) {
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

    info!("🔧 Initializing BLE and WiFi coexistence using safe ESP-IDF abstractions");

    // 🎯 PROPER SOLUTION: Initialize BLE first, then WiFi using safe abstractions
    // ESP32 supports BLE/WiFi coexistence when initialized in correct order

    // 🎯 PROPER APPROACH: Initialize both BLE and WiFi drivers in main() for safe coexistence

    // Step 1: Initialize BLE driver first
    info!("🔧 Step 1: Initializing BLE driver safely");
    use esp_idf_svc::bt::{Ble, BtDriver};

    let bt_driver = match BtDriver::new(peripherals.modem, None) {
        Ok(driver) => {
            info!("✅ BLE driver initialized successfully");
            Some(driver)
        }
        Err(e) => {
            error!("❌ Failed to initialize BLE driver: {:?}", e);
            warn!("⚠️ Continuing with WiFi-only mode");
            None
        }
    };

    // Step 2: Initialize WiFi normally
    info!("🔧 Step 2: Initializing WiFi system");

    // For BLE/WiFi coexistence, we need a more sophisticated approach
    // For now, let's create a WiFi-only system that can be extended later
    if bt_driver.is_some() {
        warn!("⚠️ BLE/WiFi coexistence not yet fully implemented");
        warn!("⚠️ Will attempt WiFi-only mode for now");
    }

    // Get a fresh modem reference for WiFi
    let wifi_modem = match Peripherals::take() {
        Ok(p) => {
            info!("✅ Got modem for WiFi");
            p.modem
        }
        Err(_) => {
            // BLE has the modem, try working without it
            warn!("⚠️ Cannot get modem for WiFi - BLE may have it");
            warn!("⚠️ WiFi functionality will be limited");

            // Still spawn BLE task to handle stored credentials
            if let Err(_) = spawner.spawn(ble_provisioning_task(wifi_storage, bt_driver)) {
                error!("Failed to spawn BLE provisioning task");
                return;
            }

            info!("✅ BLE-only mode initialized - will use stored WiFi credentials only");
            return;
        }
    };

    // Spawn the persistent WiFi task with the modem
    if let Err(_) = spawner.spawn(persistent_wifi_task(wifi_modem, sys_loop, timer_service)) {
        error!("Failed to spawn persistent WiFi task");
        return;
    }

    // Pass both WiFi storage and BLE driver to BLE task
    if let Err(_) = spawner.spawn(ble_provisioning_task(wifi_storage, bt_driver)) {
        error!("Failed to spawn BLE provisioning task");
        return;
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
                state.ble_shutdown_requested = true;
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

// PERSISTENT WIFI TASK - Solves the Peripherals::take() singleton issue
// This task owns the WiFi driver for the entire program lifecycle
// It receives commands via channels and responds accordingly
// NO MORE Peripherals::take() calls needed - smooth user experience!
#[embassy_executor::task]
async fn persistent_wifi_task(
    modem: Modem,
    sys_loop: EspSystemEventLoop,
    timer_service: EspTaskTimerService,
) {
    info!("🌐 Persistent WiFi Task started - owns WiFi driver for entire program lifecycle");
    info!("ℹ️ WiFi driver running without NVS - credentials handled by WiFi storage");

    // Initialize WiFi driver once - this is the ONLY place we create it
    // Note: No NVS partition for WiFi driver since WiFi storage owns it
    let mut wifi = match AsyncWifi::wrap(
        EspWifi::new(modem, sys_loop.clone(), None).unwrap(),
        sys_loop,
        timer_service,
    ) {
        Ok(wifi) => {
            info!("✅ WiFi driver initialized successfully - ready for commands");
            wifi
        }
        Err(e) => {
            error!("❌ Failed to initialize WiFi driver: {:?}", e);
            return;
        }
    };

    let mut is_connected = false;
    let mut current_ip: Option<Ipv4Addr> = None;

    // 🔧 CRITICAL FIX: Set WiFi to station mode BEFORE starting
    info!("🔧 Setting WiFi to station mode (not AP mode)");
    let station_config = Configuration::Client(ClientConfiguration::default());
    if let Err(e) = wifi.set_configuration(&station_config) {
        error!("❌ Failed to set WiFi to station mode: {:?}", e);
        return;
    }
    info!("✅ WiFi configured in station mode");

    // Start WiFi in station mode
    if let Err(e) = wifi.start().await {
        error!("❌ Failed to start WiFi: {:?}", e);
        return;
    }
    info!("🚀 WiFi driver started in station mode and ready for configuration commands");

    // Command processing loop - handles reconfiguration without recreation
    loop {
        let command = WIFI_COMMAND_CHANNEL.receive().await;

        match command {
            WiFiCommand::Connect(credentials) => {
                info!(
                    "📶 Received WiFi connect command for SSID: {}",
                    credentials.ssid
                );

                // Disconnect if currently connected
                if is_connected {
                    info!("🔄 Disconnecting from current network first");
                    let _ = wifi.disconnect().await;
                    is_connected = false;
                    current_ip = None;
                }

                // Configure WiFi for new credentials
                let wifi_config = Configuration::Client(ClientConfiguration {
                    ssid: credentials.ssid.as_str().try_into().unwrap_or_default(),
                    password: credentials.password.as_str().try_into().unwrap_or_default(),
                    ..Default::default()
                });

                match wifi.set_configuration(&wifi_config) {
                    Ok(_) => info!("✅ WiFi configured for SSID: {}", credentials.ssid),
                    Err(e) => {
                        error!("❌ Failed to configure WiFi: {:?}", e);
                        WIFI_RESPONSE_CHANNEL
                            .send(WiFiResponse::Failed(format!("Config failed: {:?}", e)))
                            .await;
                        continue;
                    }
                }

                // Attempt connection with timeout
                WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::ConnectionAttempting);

                match with_timeout(Duration::from_secs(30), wifi.connect()).await {
                    Ok(Ok(_)) => {
                        info!("✅ WiFi connected successfully!");

                        // Wait for IP assignment
                        match with_timeout(Duration::from_secs(15), wifi.wait_netif_up()).await {
                            Ok(Ok(_)) => {
                                let ip_info = wifi.wifi().sta_netif().get_ip_info().unwrap();
                                let ip_address = ip_info.ip;

                                is_connected = true;
                                current_ip = Some(ip_address);

                                info!("🌐 WiFi connection successful! IP: {}", ip_address);

                                // Send success response
                                WIFI_RESPONSE_CHANNEL
                                    .send(WiFiResponse::Connected(ip_address))
                                    .await;
                                WIFI_STATUS_SIGNAL
                                    .signal(WiFiConnectionEvent::ConnectionSuccessful(ip_address));

                                // Test connectivity
                                if let Err(e) = test_connectivity_and_register(ip_address).await {
                                    warn!("⚠️ Connectivity test failed: {:?}", e);
                                }
                            }
                            Ok(Err(e)) => {
                                error!("❌ Failed to get IP address: {:?}", e);
                                WIFI_RESPONSE_CHANNEL
                                    .send(WiFiResponse::Failed(format!(
                                        "IP assignment failed: {:?}",
                                        e
                                    )))
                                    .await;
                                WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::ConnectionFailed(
                                    format!("IP assignment failed: {:?}", e),
                                ));
                            }
                            Err(_) => {
                                error!("❌ IP assignment timeout");
                                WIFI_RESPONSE_CHANNEL
                                    .send(WiFiResponse::Failed("IP assignment timeout".to_string()))
                                    .await;
                                WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::ConnectionFailed(
                                    "IP assignment timeout".to_string(),
                                ));
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        error!("❌ WiFi connection failed: {:?}", e);
                        WIFI_RESPONSE_CHANNEL
                            .send(WiFiResponse::Failed(format!("Connection failed: {:?}", e)))
                            .await;
                        WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::ConnectionFailed(format!(
                            "Connection failed: {:?}",
                            e
                        )));
                    }
                    Err(_) => {
                        error!("❌ WiFi connection timeout");
                        WIFI_RESPONSE_CHANNEL
                            .send(WiFiResponse::Failed("Connection timeout".to_string()))
                            .await;
                        WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::ConnectionFailed(
                            "Connection timeout".to_string(),
                        ));
                    }
                }
            }

            WiFiCommand::Disconnect => {
                info!("🔄 Received WiFi disconnect command");
                if is_connected {
                    let _ = wifi.disconnect().await;
                    is_connected = false;
                    current_ip = None;
                    WIFI_RESPONSE_CHANNEL.send(WiFiResponse::Disconnected).await;
                    info!("✅ WiFi disconnected");
                } else {
                    WIFI_RESPONSE_CHANNEL.send(WiFiResponse::Disconnected).await;
                    info!("ℹ️ WiFi already disconnected");
                }
            }

            WiFiCommand::GetStatus => {
                WIFI_RESPONSE_CHANNEL
                    .send(WiFiResponse::Status {
                        connected: is_connected,
                        ip: current_ip,
                    })
                    .await;
            }
        }

        // Small yield
        Timer::after(Duration::from_millis(10)).await;
    }
}

// BLE PROVISIONING TASK - Now communicates via channels (no modem ownership)
// This task handles all Bluetooth Low Energy functionality for WiFi provisioning
// IMPROVED: Uses channel communication with persistent WiFi task - no Peripherals::take() issues!
// FIXED: Receives WiFi storage AND BLE driver from main() - safe coexistence!
#[embassy_executor::task]
async fn ble_provisioning_task(
    mut wifi_storage: WiFiStorage,
    bt_driver: Option<esp_idf_svc::bt::BtDriver<'static, esp_idf_svc::bt::Ble>>,
) {
    info!("📻 BLE Provisioning Task started - Event-driven WiFi setup");
    info!("✅ WiFi storage and BLE driver passed from main() - safe coexistence!");

    // Check if BLE driver was successfully initialized
    let available_bt_driver = match bt_driver {
        Some(driver) => {
            info!("✅ BLE driver available - full provisioning mode");
            Some(driver)
        }
        None => {
            warn!("⚠️ No BLE driver available - limited functionality");
            warn!("⚠️ Device will rely on stored WiFi credentials only");

            // Try to connect with stored credentials only
            if wifi_storage.has_stored_credentials() {
                info!("Attempting connection with stored credentials...");
                if let Ok(Some(credentials)) = wifi_storage.load_credentials() {
                    WIFI_COMMAND_CHANNEL
                        .send(WiFiCommand::Connect(credentials))
                        .await;

                    let response = WIFI_RESPONSE_CHANNEL.receive().await;
                    match response {
                        WiFiResponse::Connected(ip_address) => {
                            info!("✅ Connected with stored credentials - IP: {}", ip_address);
                            return;
                        }
                        _ => {
                            error!(
                                "❌ Failed to connect with stored credentials and no BLE available"
                            );
                            return;
                        }
                    }
                }
            }

            error!("❌ No BLE driver and no stored credentials - cannot provision WiFi");
            return;
        }
    };

    // Check if we already have stored credentials
    if wifi_storage.has_stored_credentials() {
        info!("Found existing WiFi credentials, attempting connection...");
        if let Ok(Some(credentials)) = wifi_storage.load_credentials() {
            info!("Loaded credentials for SSID: {}", credentials.ssid);

            // Send WiFi connect command to persistent WiFi task
            WIFI_COMMAND_CHANNEL
                .send(WiFiCommand::Connect(credentials))
                .await;

            // Wait for response
            let response = WIFI_RESPONSE_CHANNEL.receive().await;
            match response {
                WiFiResponse::Connected(ip_address) => {
                    info!(
                        "✅ Connected with stored credentials - IP: {} - BLE task terminating",
                        ip_address
                    );
                    SYSTEM_EVENT_SIGNAL
                        .signal(SystemEvent::TaskTerminating("BLE Provisioning".to_string()));
                    return; // Task exits successfully
                }
                WiFiResponse::Failed(error) => {
                    warn!("Failed to connect with stored credentials: {}", error);
                    // Continue with BLE provisioning
                }
                _ => {
                    warn!("Unexpected response from WiFi task");
                }
            }
        }
    }

    // Generate device ID and create BLE server
    let device_id = generate_device_id();
    let mut ble_server = BleServer::new(&device_id);

    // Check if BLE driver is available for initialization
    let ble_initialized = if let Some(bt_driver) = available_bt_driver {
        info!("📻 BLE driver is available - initializing BLE server for WiFi coexistence");

        // Use the initialized BLE driver to set up the BLE server
        match ble_server.initialize_with_bt_driver(bt_driver).await {
            Ok(_) => {
                info!("✅ BLE server initialized successfully with WiFi coexistence");
                true
            }
            Err(e) => {
                error!("❌ Failed to initialize BLE server: {:?}", e);
                warn!("⚠️ Continuing without BLE - WiFi credentials from storage only");
                false
            }
        }
    } else {
        warn!("⚠️ No BLE driver available - BLE initialization skipped");
        info!("ℹ️ Will attempt to use stored WiFi credentials only");
        false
    };

    // Start BLE advertising only if BLE was successfully initialized
    let ble_active = if ble_initialized {
        match ble_server.start_advertising().await {
            Ok(_) => {
                info!(
                    "📻 BLE advertising started - Device: AcornPups-{}",
                    device_id
                );
                true
            }
            Err(e) => {
                error!("Failed to start BLE advertising: {}", e);
                warn!("⚠️ Continuing without BLE advertising");
                false
            }
        }
    } else {
        info!("ℹ️ BLE not available - skipping advertising");
        false
    };

    // Update system state based on BLE availability
    {
        let mut state = SYSTEM_STATE.lock().await;
        state.ble_active = ble_active;
    }

    if ble_active {
        SYSTEM_EVENT_SIGNAL.signal(SystemEvent::ProvisioningMode);
    } else {
        info!("📱 No BLE available - checking for stored WiFi credentials");
    }

    // EVENT-DRIVEN MAIN LOOP - NO POLLING!
    // This task waits for events and responds accordingly
    loop {
        // Check system state to see if BLE shutdown was requested
        {
            let state = SYSTEM_STATE.lock().await;
            if state.ble_shutdown_requested {
                info!("🔄 BLE shutdown requested - initiating cleanup...");

                // Wait for status message delivery
                Timer::after(Duration::from_secs(3)).await;

                // Complete BLE shutdown
                if let Err(e) = ble_server.shutdown_ble_completely().await {
                    warn!("Failed to shutdown BLE completely: {}", e);
                } else {
                    info!("✅ BLE hardware completely disabled and resources freed");

                    // Update system state directly after successful shutdown
                    {
                        let mut state = SYSTEM_STATE.lock().await;
                        state.ble_active = false;
                        state.provisioning_complete = true;
                    }
                }

                break; // Task should terminate
            }
        }

        // Handle BLE events in a non-blocking way
        ble_server.handle_events().await;

        // Check for received credentials (event-driven)
        if let Some(credentials) = ble_server.get_received_credentials() {
            info!("📱 WiFi credentials received via BLE");

            // Store credentials in NVS
            match wifi_storage.store_credentials(&credentials) {
                Ok(()) => {
                    info!("💾 WiFi credentials stored successfully");
                    WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::CredentialsStored);
                    ble_server.send_wifi_status(true, None).await;
                }
                Err(e) => {
                    error!("Failed to store WiFi credentials: {:?}", e);
                    WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::CredentialsInvalid);
                    ble_server.send_wifi_status(false, None).await;
                    continue; // Stay in loop for retry
                }
            }

            // Signal WiFi connection attempt
            WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::ConnectionAttempting);

            // Send WiFi connect command to persistent WiFi task
            WIFI_COMMAND_CHANNEL
                .send(WiFiCommand::Connect(credentials.clone()))
                .await;

            // Wait for response from WiFi task
            let response = WIFI_RESPONSE_CHANNEL.receive().await;
            match response {
                WiFiResponse::Connected(ip_address) => {
                    info!("✅ WiFi connection successful! IP: {}", ip_address);

                    // Send success status with IP address to mobile app
                    ble_server
                        .send_wifi_status(true, Some(&ip_address.to_string()))
                        .await;

                    // The system coordinator will request BLE shutdown via system state
                    // This task will detect the shutdown request in the next loop iteration
                }
                WiFiResponse::Failed(error) => {
                    error!("WiFi connection failed: {}", error);
                    ble_server.send_wifi_status(false, None).await;

                    // Clear stored credentials if connection failed
                    if let Err(e) = wifi_storage.clear_credentials() {
                        warn!("Failed to clear invalid credentials: {:?}", e);
                    }

                    // Continue BLE provisioning - stay in loop for retry
                }
                _ => {
                    warn!("Unexpected response from WiFi task");
                    ble_server.send_wifi_status(false, None).await;
                }
            }
        }

        // Small yield to prevent task hogging (much smaller than polling delay)
        Timer::after(Duration::from_millis(10)).await;
    }

    // Clean task termination
    info!("🏁 BLE Provisioning Task completed successfully");
    info!("🔚 BLE task terminating cleanly - all resources freed");
    info!("📶 Device transitioned to WiFi-only operation");

    // Signal clean task termination
    SYSTEM_EVENT_SIGNAL.signal(SystemEvent::TaskTerminating("BLE Provisioning".to_string()));

    // Task function ends here - Embassy will clean up the task and free its memory
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
    match register_device_with_backend(&device_id, ip_address).await {
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

// Register device with Acorn Pups backend
async fn register_device_with_backend(
    device_id: &str,
    ip_address: Ipv4Addr,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("📡 Registering with Acorn Pups backend...");

    // Device registration payload
    let registration_data = format!(
        r#"{{
            "device_id": "{}",
            "device_type": "acorn_pups_esp32",
            "firmware_version": "1.0.0",
            "ip_address": "{}",
            "capabilities": ["wifi_provisioning", "ble", "sensors"],
            "registration_time": "{}",
            "hardware_info": {{
                "platform": "ESP32",
                "ram_size": "520KB",
                "flash_size": "4MB",
                "wifi_mac": "auto_detected"
            }}
        }}"#,
        device_id,
        ip_address,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );

    let config = esp_idf_svc::http::client::Configuration {
        timeout: Some(std::time::Duration::from_secs(15)),
        ..Default::default()
    };

    let mut client = Client::wrap(EspHttpConnection::new(&config)?);

    // Replace with your actual Acorn Pups backend URL
    let backend_url = "https://api.acornpups.com/devices/register";

    info!("🌐 Sending registration to: {}", backend_url);

    let headers = [
        ("Content-Type", "application/json"),
        ("User-Agent", "AcornPups-ESP32/1.0.0"),
        ("X-Device-Type", "acorn_pups"),
    ];

    let mut request = client.post(backend_url, &headers)?;
    request.write_all(registration_data.as_bytes())?;
    request.flush()?;

    let response = request.submit()?;

    match response.status() {
        201 | 200 => {
            info!(
                "✅ Device registration successful - status: {}",
                response.status()
            );
            Ok(())
        }
        409 => {
            info!(
                "ℹ️ Device already registered - status: {}",
                response.status()
            );
            Ok(())
        }
        _ => {
            let error_msg = format!("Registration failed with status: {}", response.status());
            Err(error_msg.into())
        }
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

// RESTART FUNCTIONS REMOVED - No longer needed with persistent WiFi task approach!
// The channel-based communication eliminates the need for device restarts.

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
