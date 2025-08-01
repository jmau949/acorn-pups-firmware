// Import the Embassy executor's Spawner type - this allows us to create async tasks
// Embassy is an async runtime for embedded systems, like Tokio but for microcontrollers
use embassy_executor::Spawner;


// Import Embassy time utilities for delays and timers
// Duration represents a time span, Timer provides async delays (non-blocking waits)
use embassy_time::{Duration, Timer};



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



// Import async runtime components
use embassy_time::with_timeout;

// Import logging macros for debug output over serial/UART
// error! = critical errors, info! = general information, warn! = warnings
use log::{error, info, warn};




// Declare our custom modules (separate files in src/ directory)
// Each mod statement tells Rust to include code from src/module_name.rs
mod api; // HTTP API client for REST communication
mod ble_server; // Bluetooth Low Energy server functionality
mod connectivity; // Device connectivity and registration
mod device_api; // Device-specific API client for ESP32 receivers
mod device_info; // Device information utilities
mod led_manager; // LED status indicator management
mod mqtt_certificates; // Secure storage of AWS IoT Core certificates
mod mqtt_client; // AWS IoT Core MQTT client with TLS authentication
mod mqtt_manager; // Embassy task coordination for MQTT operations
mod nvs_debug; // NVS flash storage debugging utilities
mod reset_handler; // Reset behavior execution
mod reset_manager; // GPIO reset button monitoring and state management
mod settings; // Device settings management with MQTT and NVS integration
mod system_state; // System state management and event coordination
mod volume_control; // Volume control with GPIO buttons and MQTT integration
mod wifi_storage; // Persistent storage of WiFi credentials

// Import specific items from our modules to use in this file
// This is like "from module import function" in Python
use ble_server::{generate_device_id, BleServer}; // BLE advertising and communication
use connectivity::test_connectivity_and_register; // Device connectivity
use led_manager::led_task; // LED status management
use mqtt_certificates::MqttCertificateStorage; // Secure certificate storage
use mqtt_manager::MqttManager; // MQTT task coordination
use nvs_debug::dump_entire_nvs_storage; // NVS debugging utilities
use reset_handler::ResetHandler; // Reset behavior execution
use reset_manager::{ResetManager, ResetManagerEvent}; // Reset button monitoring and tiered recovery
use settings::SettingsManager; // Device settings management
use system_state::{SystemEvent, WiFiConnectionEvent, SYSTEM_EVENT_SIGNAL, SYSTEM_STATE, WIFI_STATUS_SIGNAL}; // System state
use volume_control::{VolumeManager, VolumeButtonHandler}; // Volume control with GPIO buttons
use wifi_storage::WiFiStorage; // NVS flash storage for WiFi creds






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

    // Initialize volume control GPIO pins (GPIO12 for volume up, GPIO13 for volume down)
    let volume_up_gpio = peripherals.pins.gpio12;
    let volume_down_gpio = peripherals.pins.gpio13;
    info!("Volume control GPIOs allocated: GPIO12 (Volume Up), GPIO13 (Volume Down)");

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
            volume_up_gpio,
            volume_down_gpio,
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

                // Check if this is an MQTT-related error
                if error.contains("MQTT connection failed") || error.contains("MQTT") {
                    error!("🔄 MQTT-related error detected: {}", error);
                    error!("📋 MQTT manager should handle its own retries - no duplicate spawning");
                    warn!("⚠️ If MQTT issues persist, the existing MQTT manager will trigger factory reset");

                    // Don't spawn new MQTT managers - let the existing one handle retries
                    // The MQTT manager has built-in retry logic and will trigger factory reset if needed
                } else {
                    warn!("⚠️ Non-MQTT system error - continuing operation: {}", error);
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
    volume_up_gpio: esp_idf_svc::hal::gpio::Gpio12,
    volume_down_gpio: esp_idf_svc::hal::gpio::Gpio13,
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
                                volume_up_gpio,
                                volume_down_gpio,
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

// SETTINGS MANAGER TASK - Handles device settings with MQTT and NVS integration
#[embassy_executor::task]
async fn settings_manager_task(device_id: String, nvs_partition: EspDefaultNvsPartition) {
    info!("🔧 Settings Manager Task started for device: {}", device_id);

    // Initialize settings manager with NVS partition
    match SettingsManager::new_with_partition(nvs_partition, device_id.clone()) {
        Ok(mut settings_manager) => {
            info!("✅ Settings manager initialized successfully");

            // Run the settings manager main loop
            settings_manager.run_settings_task().await;
        }
        Err(e) => {
            error!("❌ Failed to initialize settings manager: {}", e);

            // Signal system error
            SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemError(format!(
                "Settings manager initialization failed: {}",
                e
            )));

            // Task remains alive but non-functional
            loop {
                Timer::after(Duration::from_secs(60)).await;
                warn!("⚠️ Settings manager task is inactive due to initialization failure");
            }
        }
    }
}

// VOLUME CONTROL MANAGER TASK - Handles physical volume buttons and volume management
#[embassy_executor::task]
async fn volume_control_manager_task(
    device_id: String,
    _nvs_partition: EspDefaultNvsPartition,
    volume_up_gpio: esp_idf_svc::hal::gpio::Gpio12,
    volume_down_gpio: esp_idf_svc::hal::gpio::Gpio13,
) {
    info!("🔊 Volume Control Manager Task started for device: {}", device_id);

    // Get initial volume from settings
    use crate::settings::{request_current_settings, SETTINGS_EVENT_SIGNAL, SettingsEvent};
    
    request_current_settings();
    let initial_volume = loop {
        let settings_event = SETTINGS_EVENT_SIGNAL.wait().await;
        if let SettingsEvent::SettingsUpdated(settings) = settings_event {
            break settings.sound_volume;
        }
        Timer::after(Duration::from_millis(100)).await;
    };

    // Initialize volume manager
    let mut volume_manager = match VolumeManager::new(device_id.clone(), initial_volume) {
        Ok(manager) => {
            info!("✅ Volume manager initialized successfully");
            manager
        }
        Err(e) => {
            error!("❌ Failed to initialize volume manager: {}", e);
            return;
        }
    };

    // Initialize button handler
    let mut button_handler = match VolumeButtonHandler::new(volume_up_gpio, volume_down_gpio) {
        Ok(handler) => {
            info!("✅ Volume button handler initialized successfully");
            handler
        }
        Err(e) => {
            error!("❌ Failed to initialize volume button handler: {}", e);
            return;
        }
    };

    // Spawn button monitoring and volume management concurrently
    use embassy_futures::select::{select, Either};

    loop {
        match select(
            button_handler.start_monitoring(),
            volume_manager.run_volume_task(),
        ).await {
            Either::First(button_result) => {
                if let Err(e) = button_result {
                    error!("❌ Volume button monitoring failed: {}", e);
                    Timer::after(Duration::from_secs(5)).await; // Recovery delay
                }
            }
            Either::Second(_) => {
                // Volume manager task completed (shouldn't normally happen)
                warn!("⚠️ Volume manager task completed unexpectedly");
                Timer::after(Duration::from_secs(5)).await;
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
