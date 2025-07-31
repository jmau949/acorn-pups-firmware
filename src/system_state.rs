use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;

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