// Development version for Wokwi simulation
// This version simulates the same logic flow without BLE/WiFi hardware calls
// but runs on ESP32 target for Wokwi visual simulation

use embassy_executor::Spawner;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use esp_idf_svc::hal::gpio::*;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use log::*;
use std::sync::LazyLock;

// Import shared types
use pup_shared::{SystemEvent, SystemState, WiFiConnectionEvent, WiFiCredentials};

// Global notification system for task coordination
static WIFI_CHANNEL: Channel<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    WiFiConnectionEvent,
    10,
> = Channel::new();
static SYSTEM_CHANNEL: Channel<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    SystemEvent,
    10,
> = Channel::new();

struct EventNotifier {
    wifi_sender: Sender<
        'static,
        embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
        WiFiConnectionEvent,
        10,
    >,
    wifi_receiver: Receiver<
        'static,
        embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
        WiFiConnectionEvent,
        10,
    >,
    system_sender: Sender<
        'static,
        embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
        SystemEvent,
        10,
    >,
    system_receiver: Receiver<
        'static,
        embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
        SystemEvent,
        10,
    >,
}

impl EventNotifier {
    fn new() -> Self {
        Self {
            wifi_sender: WIFI_CHANNEL.sender(),
            wifi_receiver: WIFI_CHANNEL.receiver(),
            system_sender: SYSTEM_CHANNEL.sender(),
            system_receiver: SYSTEM_CHANNEL.receiver(),
        }
    }

    async fn signal_wifi(&self, event: WiFiConnectionEvent) {
        let _ = self.wifi_sender.send(event).await;
    }

    async fn signal_system(&self, event: SystemEvent) {
        let _ = self.system_sender.send(event).await;
    }

    async fn wait_wifi(&self) -> WiFiConnectionEvent {
        self.wifi_receiver.receive().await
    }

    async fn wait_system(&self) -> SystemEvent {
        self.system_receiver.receive().await
    }
}

// Shared system state - protected by mutex for safe concurrent access
static SHARED_STATE: Mutex<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    SystemState,
> = Mutex::new(SystemState::new());
static SHARED_EVENTS: LazyLock<EventNotifier> = LazyLock::new(|| EventNotifier::new());

// Real GPIO pin for LEDs (for Wokwi simulation)
type LedPin = PinDriver<'static, AnyOutputPin, Output>;

fn create_led_pin(pin: AnyOutputPin) -> Result<LedPin, esp_idf_sys::EspError> {
    PinDriver::output(pin)
}

fn set_led_high(pin: &mut LedPin) -> Result<(), esp_idf_sys::EspError> {
    pin.set_high()
}

fn set_led_low(pin: &mut LedPin) -> Result<(), esp_idf_sys::EspError> {
    pin.set_low()
}

// Simulated WiFi credentials storage
struct SimulatedWiFiStorage {
    stored_credentials: Option<WiFiCredentials>,
}

impl SimulatedWiFiStorage {
    fn new() -> Self {
        Self {
            stored_credentials: None,
        }
    }

    fn has_stored_credentials(&self) -> bool {
        self.stored_credentials.is_some()
    }

    fn load_credentials(&self) -> Result<Option<WiFiCredentials>, &'static str> {
        Ok(self.stored_credentials.clone())
    }

    fn store_credentials(&mut self, credentials: &WiFiCredentials) -> Result<(), &'static str> {
        info!("💾 Simulating credential storage: {}", credentials.ssid);
        self.stored_credentials = Some(credentials.clone());
        Ok(())
    }

    fn clear_credentials(&mut self) -> Result<(), &'static str> {
        info!("🗑️ Simulating credential clearing");
        self.stored_credentials = None;
        Ok(())
    }
}

// The main function for Wokwi simulation
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize ESP32 services
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("🧪 Starting Wokwi Simulation Mode");
    info!("📱 This version simulates BLE/WiFi behavior with visual LEDs");

    // Initialize ESP32 peripherals
    let peripherals = Peripherals::take().unwrap();
    let _sys_loop = EspSystemEventLoop::take().unwrap();
    let _nvs = EspDefaultNvsPartition::take().unwrap();

    // Create LED pins for Wokwi (GPIO 2, 4, 5 as in diagram.json)
    let led_red = create_led_pin(peripherals.pins.gpio2.into()).unwrap();
    let led_green = create_led_pin(peripherals.pins.gpio4.into()).unwrap();
    let led_blue = create_led_pin(peripherals.pins.gpio5.into()).unwrap();

    info!("🔌 LEDs initialized (Red: GPIO2, Green: GPIO4, Blue: GPIO5)");

    // Create simulated WiFi storage
    let mut wifi_storage = SimulatedWiFiStorage::new();

    // 🧪 DEVELOPMENT MODE: Simulate credential clearing for testing
    info!("🧪 DEV MODE: Clearing WiFi credentials to test BLE provisioning flow");
    if let Err(e) = wifi_storage.clear_credentials() {
        warn!("⚠️ Failed to clear simulated credentials: {:?}", e);
    }

    // Signal system startup
    SHARED_EVENTS
        .signal_system(SystemEvent::SystemStartup)
        .await;

    // Spawn the system coordinator task first
    spawner.spawn(system_coordinator_task()).unwrap();

    // Spawn LED control task
    spawner
        .spawn(led_control_task(led_red, led_green, led_blue))
        .unwrap();

    // Check WiFi credentials to determine device mode (same logic as production)
    info!("🔧 Checking WiFi credentials to determine device mode...");

    if wifi_storage.has_stored_credentials() {
        info!("✅ WiFi credentials found - Starting simulated WiFi-only mode");

        // Spawn WiFi-only mode simulation
        spawner
            .spawn(simulated_wifi_only_mode_task(wifi_storage))
            .unwrap();

        info!("📶 Device operating in simulated WiFi-only mode");
    } else {
        info!("📻 No WiFi credentials found - Starting simulated BLE provisioning mode");

        // Spawn BLE provisioning mode simulation
        spawner
            .spawn(simulated_ble_provisioning_mode_task(wifi_storage))
            .unwrap();

        info!("📱 Device operating in simulated BLE provisioning mode");
    }

    info!("✅ All Wokwi simulation tasks spawned successfully");
    info!("🎛️ Event-driven architecture initialized - Wokwi simulation mode");

    // Main coordinator loop (same as production)
    loop {
        let system_event = SHARED_EVENTS.wait_system().await;

        match system_event {
            SystemEvent::SystemStartup => {
                info!("🚀 Wokwi system startup event received");
            }

            SystemEvent::ProvisioningMode => {
                info!("📲 Wokwi system entered provisioning mode");
                {
                    let mut state = SHARED_STATE.lock().await;
                    state.ble_active = true;
                    state.provisioning_complete = false;
                }
            }

            SystemEvent::WiFiMode => {
                info!("📶 Wokwi system transitioned to WiFi-only mode");
                {
                    let mut state = SHARED_STATE.lock().await;
                    state.ble_active = false;
                    state.provisioning_complete = true;
                }
                info!("🎯 Wokwi device now operating as WiFi-only device");
            }

            SystemEvent::SystemError(error) => {
                error!("💥 Wokwi system error: {}", error);
            }

            SystemEvent::TaskTerminating(task_name) => {
                info!("🔚 Wokwi task terminating cleanly: {}", task_name);
            }
        }

        Timer::after(Duration::from_millis(100)).await;
    }
}

// SYSTEM COORDINATOR TASK (same as production)
#[embassy_executor::task]
async fn system_coordinator_task() {
    info!("🎛️ Simulated System Coordinator Task started");

    loop {
        let wifi_event = SHARED_EVENTS.wait_wifi().await;
        handle_wifi_status_change(wifi_event).await;
    }
}

async fn handle_wifi_status_change(event: WiFiConnectionEvent) {
    match event {
        WiFiConnectionEvent::ConnectionAttempting => {
            info!("🔄 Simulated WiFi connection attempt in progress...");
            {
                let mut state = SHARED_STATE.lock().await;
                state.wifi_connected = false;
                state.wifi_ip = None;
            }
        }

        WiFiConnectionEvent::ConnectionSuccessful(ip) => {
            info!("✅ Simulated WiFi connection successful - IP: {}", ip);
            {
                let mut state = SHARED_STATE.lock().await;
                state.wifi_connected = true;
                state.wifi_ip = Some(ip);
            }
            SHARED_EVENTS.signal_system(SystemEvent::WiFiMode).await;
        }

        WiFiConnectionEvent::ConnectionFailed(error) => {
            warn!("❌ Simulated WiFi connection failed: {}", error);
            {
                let mut state = SHARED_STATE.lock().await;
                state.wifi_connected = false;
                state.wifi_ip = None;
            }
        }

        WiFiConnectionEvent::CredentialsStored => {
            info!("💾 Simulated WiFi credentials stored successfully");
        }

        WiFiConnectionEvent::CredentialsInvalid => {
            warn!("⚠️ Simulated invalid WiFi credentials received");
        }
    }
}

// SIMULATED WIFI-ONLY MODE TASK
#[embassy_executor::task]
async fn simulated_wifi_only_mode_task(wifi_storage: SimulatedWiFiStorage) {
    info!("📶 Simulated WiFi-Only Mode Task started");

    // Simulate loading credentials
    match wifi_storage.load_credentials() {
        Ok(Some(credentials)) => {
            info!("📶 Simulating connection to WiFi: {}", credentials.ssid);

            // Simulate connection delay
            Timer::after(Duration::from_secs(3)).await;

            // Simulate successful connection
            let simulated_ip = std::net::Ipv4Addr::new(192, 168, 1, 100);
            info!("✅ Simulated WiFi connected! IP: {}", simulated_ip);

            SHARED_EVENTS
                .signal_wifi(WiFiConnectionEvent::ConnectionSuccessful(simulated_ip))
                .await;

            // Update system state
            {
                let mut state = SHARED_STATE.lock().await;
                state.wifi_connected = true;
                state.wifi_ip = Some(simulated_ip);
                state.ble_active = false;
                state.provisioning_complete = true;
            }

            // Simulate connectivity test
            simulate_connectivity_test(simulated_ip).await;

            // Stay connected forever (simulation)
            loop {
                Timer::after(Duration::from_secs(30)).await;
                info!("📶 Simulated WiFi-only mode running - IP: {}", simulated_ip);
            }
        }
        Ok(None) => {
            error!("❌ No simulated WiFi credentials found");
            restart_simulation("No credentials found").await;
        }
        Err(e) => {
            error!("❌ Failed to load simulated WiFi credentials: {:?}", e);
            restart_simulation("Credential load error").await;
        }
    }
}

// SIMULATED BLE PROVISIONING MODE TASK
#[embassy_executor::task]
async fn simulated_ble_provisioning_mode_task(mut wifi_storage: SimulatedWiFiStorage) {
    info!("📻 Simulated BLE Provisioning Mode Task started");
    info!("📱 Simulating BLE advertising and credential reception");

    // Update system state
    {
        let mut state = SHARED_STATE.lock().await;
        state.ble_active = true;
    }

    SHARED_EVENTS
        .signal_system(SystemEvent::ProvisioningMode)
        .await;

    let mut simulated_client_connected = false;

    loop {
        // Simulate BLE advertising
        if !simulated_client_connected {
            info!("📻 Simulating BLE advertising - waiting for mobile app...");
            Timer::after(Duration::from_secs(5)).await;

            // Simulate client connection (30% chance each cycle)
            if rand::random::<f32>() < 0.3 {
                info!("📱 Simulating mobile app connection!");
                simulated_client_connected = true;

                // Update system state
                {
                    let mut state = SHARED_STATE.lock().await;
                    state.ble_client_connected = true;
                }
            }
            continue;
        }

        // Simulate credential reception (if client connected)
        info!("📲 Simulating credential reception from mobile app...");
        Timer::after(Duration::from_secs(3)).await;

        // Simulate receiving credentials
        let simulated_credentials =
            WiFiCredentials::new("SimulatedSSID".to_string(), "SimulatedPassword".to_string());

        info!(
            "📱 Simulated WiFi credentials received: {}",
            simulated_credentials.ssid
        );

        // Store credentials
        match wifi_storage.store_credentials(&simulated_credentials) {
            Ok(()) => {
                info!("💾 Simulated WiFi credentials stored successfully");

                // Simulate success response to mobile app
                info!("📱 Simulating success response to mobile app");
                Timer::after(Duration::from_secs(2)).await;

                info!("🔄 Simulating device restart to switch to WiFi mode");
                restart_simulation("Credentials stored - switching to WiFi mode").await;
                break;
            }
            Err(e) => {
                error!("Failed to store simulated WiFi credentials: {:?}", e);
                Timer::after(Duration::from_secs(1)).await;
                simulated_client_connected = false; // Reset connection for retry
            }
        }
    }
}

// Simulated device restart
async fn restart_simulation(reason: &str) {
    info!("🔄 Simulated device restart: {}", reason);
    info!("🧪 In real hardware, this would restart the ESP32");

    Timer::after(Duration::from_secs(2)).await;

    info!("🔃 Simulation: Device would restart here");
    // In simulation, we just continue running
}

// Simulated connectivity test
async fn simulate_connectivity_test(ip_address: std::net::Ipv4Addr) {
    info!("🌐 Simulating connectivity test...");
    info!("📍 Simulated Local IP: {}", ip_address);

    // Simulate HTTP test
    Timer::after(Duration::from_secs(1)).await;
    info!("✅ Simulated HTTP connectivity test passed");

    // Simulate device registration
    Timer::after(Duration::from_secs(1)).await;
    info!("✅ Simulated device registration successful");

    // Simulate heartbeat
    Timer::after(Duration::from_secs(1)).await;
    info!("✅ Simulated heartbeat sent successfully");

    info!("🎉 Simulated connectivity testing completed");
}

// LED CONTROL TASK
#[embassy_executor::task]
async fn led_control_task(mut led_red: LedPin, mut led_green: LedPin, mut led_blue: LedPin) {
    info!("🔌 LED Control Task started");

    // Start with red LEDs (startup state)
    set_led_high(&mut led_red).ok();
    set_led_low(&mut led_green).ok();
    set_led_low(&mut led_blue).ok();
    info!("🔴 LEDs set to RED - System startup");

    let mut previous_state = "startup".to_string();

    loop {
        // Check current system state (same logic as production)
        let current_state = {
            let state = SHARED_STATE.lock().await;

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
                    set_led_high(&mut led_red).ok();
                    set_led_low(&mut led_green).ok();
                    set_led_low(&mut led_blue).ok();
                    info!("🔴 LEDs set to RED - System startup");
                }

                "ble_broadcasting" => {
                    set_led_low(&mut led_red).ok();
                    set_led_low(&mut led_green).ok();
                    set_led_high(&mut led_blue).ok();
                    info!("🔵 LEDs set to BLUE - BLE broadcasting");
                }

                "ble_connected" => {
                    set_led_low(&mut led_red).ok();
                    set_led_high(&mut led_green).ok();
                    set_led_low(&mut led_blue).ok();
                    info!("🟢 LEDs set to GREEN - BLE client connected");
                }

                "wifi_connected" => {
                    set_led_low(&mut led_red).ok();
                    set_led_high(&mut led_green).ok();
                    set_led_low(&mut led_blue).ok();
                    info!("🟢 LEDs set to GREEN - WiFi connected");
                }

                _ => {
                    set_led_high(&mut led_red).ok();
                    set_led_low(&mut led_green).ok();
                    set_led_low(&mut led_blue).ok();
                    warn!("🔴 LEDs set to RED - Unknown state: {}", current_state);
                }
            }

            previous_state = current_state;
        }

        Timer::after(Duration::from_millis(250)).await;
    }
}
