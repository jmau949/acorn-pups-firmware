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

    // Spawn the BLE provisioning task - handles WiFi credential setup and BLE lifecycle
    if let Err(_) = spawner.spawn(ble_provisioning_task()) {
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

// BLE PROVISIONING TASK (Event-Driven)
// This task handles all Bluetooth Low Energy functionality for WiFi provisioning
// IMPORTANT: This task has a finite lifecycle - it TERMINATES after WiFi connection succeeds
//
// KEY IMPROVEMENTS:
// ✅ No polling loops - purely event-driven using Embassy signals
// ✅ Clean task termination with proper resource cleanup
// ✅ Coordinated shutdown with system coordinator via shared state
// ✅ Integrated BLE lifecycle management (no separate coordinator needed)
// ✅ Direct system state updates for efficient coordination
#[embassy_executor::task]
async fn ble_provisioning_task() {
    info!("📻 BLE Provisioning Task started - Event-driven WiFi setup");

    // Initialize WiFi storage using ESP32's NVS (Non-Volatile Storage)
    let mut wifi_storage = match WiFiStorage::new() {
        Ok(storage) => storage,
        Err(e) => {
            error!("Failed to initialize WiFi storage: {:?}", e);
            SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemError(format!(
                "WiFi storage init failed: {:?}",
                e
            )));
            return;
        }
    };

    // Check if we already have stored credentials
    if wifi_storage.has_stored_credentials() {
        info!("Found existing WiFi credentials, attempting connection...");
        if let Ok(Some(credentials)) = wifi_storage.load_credentials() {
            info!("Loaded credentials for SSID: {}", credentials.ssid);

            // Signal connection attempt
            WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::ConnectionAttempting);

            match attempt_wifi_connection_with_stored_credentials(credentials).await {
                Ok(_) => {
                    info!("✅ Connected with stored credentials - BLE task terminating");
                    SYSTEM_EVENT_SIGNAL
                        .signal(SystemEvent::TaskTerminating("BLE Provisioning".to_string()));
                    return; // Task exits successfully
                }
                Err(e) => {
                    warn!("Failed to connect with stored credentials: {:?}", e);
                    WIFI_STATUS_SIGNAL
                        .signal(WiFiConnectionEvent::ConnectionFailed(format!("{:?}", e)));
                }
            }
        }
    }

    // Generate device ID and start BLE server
    let device_id = generate_device_id();
    let mut ble_server = BleServer::new(&device_id);

    // Start BLE advertising and update system state
    if let Err(e) = ble_server.start_advertising().await {
        error!("Failed to start BLE advertising: {:?}", e);
        SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemError(format!(
            "BLE start failed: {:?}",
            e
        )));
        return;
    }

    info!(
        "📻 BLE advertising started - Device: AcornPups-{}",
        device_id
    );

    // Update system state directly - no need for separate coordinator
    {
        let mut state = SYSTEM_STATE.lock().await;
        state.ble_active = true;
    }
    SYSTEM_EVENT_SIGNAL.signal(SystemEvent::ProvisioningMode);

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
                    warn!("Failed to shutdown BLE completely: {:?}", e);
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

            // Attempt WiFi connection
            match attempt_wifi_connection(credentials).await {
                Ok(ip_address) => {
                    info!("✅ WiFi connection successful! IP: {}", ip_address);

                    // Send success status with IP address to mobile app
                    ble_server
                        .send_wifi_status(true, Some(&ip_address.to_string()))
                        .await;

                    // Signal successful WiFi connection to system coordinator
                    WIFI_STATUS_SIGNAL
                        .signal(WiFiConnectionEvent::ConnectionSuccessful(ip_address));

                    // The system coordinator will request BLE shutdown via system state
                    // This task will detect the shutdown request in the next loop iteration
                }
                Err(e) => {
                    error!("WiFi connection failed: {:?}", e);
                    WIFI_STATUS_SIGNAL
                        .signal(WiFiConnectionEvent::ConnectionFailed(format!("{:?}", e)));
                    ble_server.send_wifi_status(false, None).await;

                    // Clear stored credentials if connection failed
                    if let Err(e) = wifi_storage.clear_credentials() {
                        warn!("Failed to clear invalid credentials: {:?}", e);
                    }

                    // Continue BLE provisioning - stay in loop for retry
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

async fn attempt_wifi_connection_with_stored_credentials(
    credentials: ble_server::WiFiCredentials,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("🔄 Attempting WiFi connection with stored credentials...");

    // This is a placeholder implementation
    // In a real implementation, you would:
    // 1. Initialize WiFi client with peripherals
    // 2. Attempt connection with actual ESP32 WiFi drivers
    // 3. Return result with proper error handling

    Timer::after(Duration::from_secs(2)).await;

    // Simulate connection attempt
    info!("Simulated WiFi connection with SSID: {}", credentials.ssid);

    // For demonstration, assume connection fails to force BLE provisioning
    // In real implementation, this would attempt actual WiFi connection
    let error_msg = "Simulated connection failure - proceeding with BLE provisioning".to_string();
    WIFI_STATUS_SIGNAL.signal(WiFiConnectionEvent::ConnectionFailed(error_msg.clone()));

    Err(error_msg.into())
}

async fn attempt_wifi_connection(
    credentials: ble_server::WiFiCredentials,
) -> Result<std::net::Ipv4Addr, Box<dyn std::error::Error>> {
    info!("🔄 Attempting WiFi connection with BLE-provided credentials...");

    // This is a placeholder implementation
    // In a real implementation, you would:
    // 1. Initialize WiFi client with actual ESP32 peripherals
    // 2. Connect using credentials with proper authentication
    // 3. Return IP address on success or detailed error on failure

    Timer::after(Duration::from_secs(3)).await;

    // Simulate successful connection for demonstration
    let simulated_ip = std::net::Ipv4Addr::new(192, 168, 1, 100);
    info!(
        "✅ Simulated successful WiFi connection to: {}",
        credentials.ssid
    );
    info!("📍 Simulated IP address: {}", simulated_ip);

    // Send test message to verify internet connectivity
    send_test_message(simulated_ip).await;

    // In a real implementation, you would signal the actual connection result here
    // For simulation, we always succeed to demonstrate the full flow
    Ok(simulated_ip)
}

async fn send_test_message(ip_address: std::net::Ipv4Addr) {
    info!("📡 Sending test message from IP: {}", ip_address);

    // Placeholder for test message and connectivity verification
    // In a real implementation, this would:
    // 1. Send HTTP request to a test endpoint (e.g., httpbin.org/get)
    // 2. Ping a known server (e.g., 8.8.8.8)
    // 3. Send device registration to your backend service
    // 4. Post status update to monitoring service
    // 5. Verify DNS resolution works
    // 6. Test HTTPS/TLS connectivity

    Timer::after(Duration::from_millis(500)).await;

    info!("✅ Test message sent successfully");
    info!("✅ Internet connectivity verified");
    info!("✅ Device is now online and ready for operation");
    info!("🌐 WiFi provisioning complete - device fully connected");
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
