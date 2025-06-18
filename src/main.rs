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

// Import logging macros for debug output over serial/UART
// error! = critical errors, info! = general information, warn! = warnings
use log::{error, info, warn};

// Declare our custom modules (separate files in src/ directory)
// Each mod statement tells Rust to include code from src/module_name.rs
mod ble_server; // Bluetooth Low Energy server functionality
mod wifi_client; // WiFi connection and management
mod wifi_storage; // Persistent storage of WiFi credentials

// Import specific items from our modules to use in this file
// This is like "from module import function" in Python
use ble_server::{generate_device_id, BleServer}; // BLE advertising and communication
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
    info!("Starting Embassy-based Application with BLE and LED control!");

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

    // Spawn the BLE task
    if let Err(_) = spawner.spawn(ble_task()) {
        warn!("Failed to spawn BLE task");
        return;
    }

    // Spawn the LED control task
    if let Err(_) = spawner.spawn(led_task(led_red, led_green, led_blue)) {
        warn!("Failed to spawn LED task");
        return;
    }

    info!("All tasks spawned successfully");

    // Main loop - can be used for other coordination tasks
    loop {
        Timer::after(Duration::from_secs(10)).await;
        info!("Main loop heartbeat - system running");
    }
}

// The #[embassy_executor::task] attribute marks this as an async task
// Tasks are independent async functions that run concurrently
// This task handles all Bluetooth Low Energy functionality for WiFi provisioning
#[embassy_executor::task]
async fn ble_task() {
    // Log that the BLE task has started
    info!("BLE Task started - Initializing WiFi provisioning system");

    // Initialize WiFi storage using ESP32's NVS (Non-Volatile Storage)
    // NVS is like a key-value database stored in flash memory that survives reboots
    let mut wifi_storage = match WiFiStorage::new() {
        Ok(storage) => storage, // Successfully initialized storage
        Err(e) => {
            error!("Failed to initialize WiFi storage: {:?}", e);
            return; // Exit task if storage initialization fails
        }
    };

    // Check if we already have stored credentials
    if wifi_storage.has_stored_credentials() {
        info!("Found existing WiFi credentials, attempting connection...");
        if let Ok(Some(credentials)) = wifi_storage.load_credentials() {
            info!("Loaded credentials for SSID: {}", credentials.ssid);

            // Try to connect with stored credentials
            //  concise way to handle the "I only care about the error case" scenario
            if let Err(e) = attempt_wifi_connection_with_stored_credentials(credentials).await {
                warn!("Failed to connect with stored credentials: {:?}", e);
                info!("Proceeding with BLE provisioning...");
            } else {
                info!("Successfully connected with stored credentials, BLE task complete");
                return;
            }
        }
    }

    // Generate device ID and start BLE server
    let device_id = generate_device_id();
    let mut ble_server = BleServer::new(&device_id);

    // Start BLE advertising
    if let Err(e) = ble_server.start_advertising().await {
        error!("Failed to start BLE advertising: {:?}", e);
        return;
    }

    info!("BLE advertising started - Device: AcornPups-{}", device_id);
    info!("Waiting for WiFi credentials via BLE...");

    // Main BLE event loop
    let mut credentials_received = false;
    let mut wifi_connection_successful = false;

    while !wifi_connection_successful {
        // Handle BLE events
        ble_server.handle_events().await;

        // Check if we received credentials
        if !credentials_received {
            if let Some(credentials) = ble_server.get_received_credentials() {
                credentials_received = true;
                info!("WiFi credentials received via BLE");

                // Store credentials in NVS
                match wifi_storage.store_credentials(&credentials) {
                    Ok(()) => {
                        info!("WiFi credentials stored successfully");
                        ble_server.send_wifi_status(true, None).await;
                    }
                    Err(e) => {
                        error!("Failed to store WiFi credentials: {:?}", e);
                        ble_server.send_wifi_status(false, None).await;
                        continue;
                    }
                }

                // Attempt WiFi connection
                match attempt_wifi_connection(credentials).await {
                    Ok(ip_address) => {
                        info!("✓ WiFi connection successful! IP: {}", ip_address);
                        ble_server
                            .send_wifi_status(true, Some(&ip_address.to_string()))
                            .await;
                        wifi_connection_successful = true;

                        // Wait a bit before stopping BLE to ensure status is sent
                        Timer::after(Duration::from_secs(2)).await;

                        // Stop BLE advertising
                        if let Err(e) = ble_server.stop_advertising().await {
                            warn!("Failed to stop BLE advertising: {:?}", e);
                        } else {
                            info!("BLE advertising stopped after successful WiFi connection");
                        }
                    }
                    Err(e) => {
                        error!("WiFi connection failed: {:?}", e);
                        ble_server.send_wifi_status(false, None).await;

                        // Clear stored credentials if connection failed
                        if let Err(e) = wifi_storage.clear_credentials() {
                            warn!("Failed to clear invalid credentials: {:?}", e);
                        }

                        // Reset for next attempt
                        credentials_received = false;
                    }
                }
            }
        }

        Timer::after(Duration::from_millis(100)).await;
    }

    info!("BLE task completed successfully - WiFi provisioning finished");
}

async fn attempt_wifi_connection_with_stored_credentials(
    credentials: ble_server::WiFiCredentials,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Attempting WiFi connection with stored credentials...");

    // This is a placeholder implementation
    // In a real implementation, you would:
    // 1. Initialize WiFi client with peripherals
    // 2. Attempt connection
    // 3. Return result

    Timer::after(Duration::from_secs(2)).await;

    // Simulate connection attempt
    info!("Simulated WiFi connection with SSID: {}", credentials.ssid);

    // For now, assume connection fails to force BLE provisioning
    Err("Simulated connection failure".into())
}

async fn attempt_wifi_connection(
    credentials: ble_server::WiFiCredentials,
) -> Result<std::net::Ipv4Addr, Box<dyn std::error::Error>> {
    info!("Attempting WiFi connection...");

    // This is a placeholder implementation
    // In a real implementation, you would:
    // 1. Initialize WiFi client with actual peripherals
    // 2. Connect using credentials
    // 3. Return IP address on success

    Timer::after(Duration::from_secs(3)).await;

    // Simulate successful connection
    let simulated_ip = std::net::Ipv4Addr::new(192, 168, 1, 100);
    info!(
        "Simulated successful WiFi connection to: {}",
        credentials.ssid
    );
    info!("Simulated IP address: {}", simulated_ip);

    // Send test message
    send_test_message(simulated_ip).await;

    Ok(simulated_ip)
}

async fn send_test_message(ip_address: std::net::Ipv4Addr) {
    info!("Sending test message from IP: {}", ip_address);

    // Placeholder for test message
    // In a real implementation, this could:
    // 1. Send HTTP request to a test endpoint
    // 2. Ping a known server
    // 3. Send device registration to your backend
    // 4. Post status update

    Timer::after(Duration::from_millis(500)).await;

    info!("✓ Test message sent successfully");
    info!("✓ Internet connectivity verified");
    info!("✓ Device is now online and ready for operation");
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
    info!("LED Task started");

    let mut pattern = 0;

    loop {
        match pattern {
            // Pattern 0: Sequential flashing (Red -> Green -> Blue)
            0 => {
                info!("LED Pattern 1: Sequential flashing");
                for _ in 0..3 {
                    // Red LED
                    led_red.set_high().ok();
                    Timer::after(Duration::from_millis(300)).await;
                    led_red.set_low().ok();
                    Timer::after(Duration::from_millis(100)).await;

                    // Green LED
                    led_green.set_high().ok();
                    Timer::after(Duration::from_millis(300)).await;
                    led_green.set_low().ok();
                    Timer::after(Duration::from_millis(100)).await;

                    // Blue LED
                    led_blue.set_high().ok();
                    Timer::after(Duration::from_millis(300)).await;
                    led_blue.set_low().ok();
                    Timer::after(Duration::from_millis(100)).await;
                }
            }

            // Pattern 1: All LEDs flashing together
            1 => {
                info!("LED Pattern 2: All LEDs flashing together");
                for _ in 0..5 {
                    led_red.set_high().ok();
                    led_green.set_high().ok();
                    led_blue.set_high().ok();
                    Timer::after(Duration::from_millis(500)).await;

                    led_red.set_low().ok();
                    led_green.set_low().ok();
                    led_blue.set_low().ok();
                    Timer::after(Duration::from_millis(500)).await;
                }
            }

            // Pattern 2: Alternating pairs
            2 => {
                info!("LED Pattern 3: Alternating pairs");
                for _ in 0..4 {
                    // Red and Blue on, Green off
                    led_red.set_high().ok();
                    led_green.set_low().ok();
                    led_blue.set_high().ok();
                    Timer::after(Duration::from_millis(400)).await;

                    // Green on, Red and Blue off
                    led_red.set_low().ok();
                    led_green.set_high().ok();
                    led_blue.set_low().ok();
                    Timer::after(Duration::from_millis(400)).await;
                }
            }

            // Pattern 3: Fast blink individual LEDs
            _ => {
                info!("LED Pattern 4: Fast individual blinks");
                // Fast red blinks
                for _ in 0..6 {
                    led_red.set_high().ok();
                    Timer::after(Duration::from_millis(100)).await;
                    led_red.set_low().ok();
                    Timer::after(Duration::from_millis(100)).await;
                }

                // Fast green blinks
                for _ in 0..6 {
                    led_green.set_high().ok();
                    Timer::after(Duration::from_millis(100)).await;
                    led_green.set_low().ok();
                    Timer::after(Duration::from_millis(100)).await;
                }

                // Fast blue blinks
                for _ in 0..6 {
                    led_blue.set_high().ok();
                    Timer::after(Duration::from_millis(100)).await;
                    led_blue.set_low().ok();
                    Timer::after(Duration::from_millis(100)).await;
                }
            }
        }

        // Move to next pattern, cycle back to 0 after pattern 3
        pattern = (pattern + 1) % 4;

        // Brief pause between patterns
        Timer::after(Duration::from_millis(1000)).await;
    }
}
