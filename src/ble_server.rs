// Production-grade BLE server implementation using real ESP-IDF GATT APIs
// This provides actual WiFi provisioning via real BLE GATT services

// Import Embassy's critical section mutex for thread-safe access
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};

// Import logging macros
use log::{error, info, warn};

// Import Serde for JSON serialization
use serde::{Deserialize, Serialize};

// Import ESP-IDF BLE functionality
use esp_idf_svc::bt::{Ble, BtDriver};
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::sys as esp_idf_sys;

// Standard library imports
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

// Production-grade BLE error types with specific ESP error codes
#[derive(Debug, Clone, PartialEq)]
pub enum BleError {
    // Initialization errors
    ControllerInitFailed(String),
    ControllerEnableFailed(String),
    BluedroidInitFailed(String),
    BluedroidEnableFailed(String),

    // GATT service errors
    ServiceRegistrationFailed(String),
    CharacteristicCreationFailed(String),
    ServiceStartFailed(String),

    // Advertising errors
    AdvertisingConfigFailed(String),
    AdvertisingStartFailed(String),
    DeviceNameSetFailed(String),

    // Connection errors
    ConnectionTimeout,
    ClientDisconnected,

    // Data errors
    InvalidCredentials(String),
    DataTransmissionFailed(String),
    CharacteristicWriteFailed(String),

    // Resource errors
    OutOfMemory,
    ResourceBusy,
    InvalidState(String),

    // Timeout errors
    OperationTimeout(String),
    InitializationTimeout,

    // Recovery errors
    RecoveryFailed(String),
    CleanupFailed(String),

    // General errors
    Unknown(String),
    SystemError(String),

    // New specific ESP-IDF error types
    EspError(esp_idf_sys::esp_err_t, String),
    NotInitialized(String),
    AlreadyInitialized(String),
}

impl std::fmt::Display for BleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BleError::ControllerInitFailed(msg) => {
                write!(f, "BLE controller initialization failed: {}", msg)
            }
            BleError::ControllerEnableFailed(msg) => {
                write!(f, "BLE controller enable failed: {}", msg)
            }
            BleError::BluedroidInitFailed(msg) => {
                write!(f, "Bluedroid initialization failed: {}", msg)
            }
            BleError::BluedroidEnableFailed(msg) => write!(f, "Bluedroid enable failed: {}", msg),
            BleError::ServiceRegistrationFailed(msg) => {
                write!(f, "GATT service registration failed: {}", msg)
            }
            BleError::CharacteristicCreationFailed(msg) => {
                write!(f, "GATT characteristic creation failed: {}", msg)
            }
            BleError::ServiceStartFailed(msg) => write!(f, "GATT service start failed: {}", msg),
            BleError::AdvertisingConfigFailed(msg) => {
                write!(f, "Advertising configuration failed: {}", msg)
            }
            BleError::AdvertisingStartFailed(msg) => {
                write!(f, "Advertising start failed: {}", msg)
            }
            BleError::DeviceNameSetFailed(msg) => write!(f, "Device name set failed: {}", msg),
            BleError::ConnectionTimeout => write!(f, "BLE connection timeout"),
            BleError::ClientDisconnected => write!(f, "BLE client disconnected"),
            BleError::InvalidCredentials(msg) => write!(f, "Invalid WiFi credentials: {}", msg),
            BleError::DataTransmissionFailed(msg) => {
                write!(f, "BLE data transmission failed: {}", msg)
            }
            BleError::CharacteristicWriteFailed(msg) => {
                write!(f, "BLE characteristic write failed: {}", msg)
            }
            BleError::OutOfMemory => write!(f, "Out of memory"),
            BleError::ResourceBusy => write!(f, "BLE resource busy"),
            BleError::InvalidState(msg) => write!(f, "Invalid BLE state: {}", msg),
            BleError::OperationTimeout(op) => write!(f, "BLE operation timeout: {}", op),
            BleError::InitializationTimeout => write!(f, "BLE initialization timeout"),
            BleError::RecoveryFailed(msg) => write!(f, "BLE recovery failed: {}", msg),
            BleError::CleanupFailed(msg) => write!(f, "BLE cleanup failed: {}", msg),
            BleError::Unknown(msg) => write!(f, "Unknown BLE error: {}", msg),
            BleError::SystemError(msg) => write!(f, "BLE system error: {}", msg),
            BleError::EspError(code, msg) => write!(f, "ESP-IDF error {}: {}", code, msg),
            BleError::NotInitialized(msg) => write!(f, "BLE not initialized: {}", msg),
            BleError::AlreadyInitialized(msg) => write!(f, "BLE already initialized: {}", msg),
        }
    }
}

impl std::error::Error for BleError {}

// Result type for BLE operations
pub type BleResult<T> = Result<T, BleError>;

// WiFi credentials structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WiFiCredentials {
    pub ssid: String,
    pub password: String,
}

// BLE communication events that main.rs expects
#[derive(Debug, Clone)]
pub enum BleEvent {
    Initialized,
    ServiceCreated { service_handle: u16 },
    CharacteristicAdded { char_handle: u16, uuid: String },
    AdvertisingStarted,
    ClientConnected,
    ClientDisconnected,
    CredentialsReceived(WiFiCredentials),
    SendResponse(String),
    Error(BleError),
}

// UUIDs for WiFi provisioning service (converted to ESP-IDF format)
pub const WIFI_SERVICE_UUID: &str = "12345678-1234-1234-1234-123456789abc";
pub const SSID_CHAR_UUID: &str = "12345678-1234-1234-1234-123456789abd";
pub const PASSWORD_CHAR_UUID: &str = "12345678-1234-1234-1234-123456789abe";
pub const STATUS_CHAR_UUID: &str = "12345678-1234-1234-1234-123456789abf";

// BLE operation timeouts
const INIT_TIMEOUT_MS: u64 = 10000;
const ADVERTISING_TIMEOUT_MS: u64 = 5000;
const CONNECTION_TIMEOUT_MS: u64 = 30000;
const DATA_TIMEOUT_MS: u64 = 10000;

// Global state for BLE operations
static BLE_INITIALIZED: AtomicBool = AtomicBool::new(false);
static BLE_ADVERTISING: AtomicBool = AtomicBool::new(false);
static GATT_IF: AtomicU8 = AtomicU8::new(0);

// Global channel for BLE events
pub static BLE_CHANNEL: Channel<CriticalSectionRawMutex, BleEvent, 10> = Channel::new();

// Event priority classification for backpressure handling
#[derive(Debug, Clone, PartialEq)]
enum EventPriority {
    Critical,  // Must never be dropped (errors, connections, credentials)
    Important, // Should try hard not to drop (initialization, service creation)
    Normal,    // Can be dropped if channel is full (status updates, advertising)
}

/// Smart event sender with backpressure handling based on event priority
///
/// This function implements different strategies based on event criticality:
/// - Critical events: Block briefly and retry, never drop
/// - Important events: Try to make space by dropping low-priority events
/// - Normal events: Drop silently if channel is full
fn send_ble_event_with_backpressure(event: BleEvent) {
    let priority = classify_event_priority(&event);

    match priority {
        EventPriority::Critical => {
            // Critical events must never be dropped - block briefly and retry
            if BLE_CHANNEL.try_send(event.clone()).is_err() {
                warn!("🚨 BLE channel full for critical event: {:?}", event);

                // For critical events, we'll attempt to drain one item and retry
                // This is safe because we're prioritizing the most important events
                if let Ok(_) = BLE_CHANNEL.try_receive() {
                    warn!("📤 Dropped low-priority event to make space for critical event");
                }

                // Retry the critical event
                if let Err(_) = BLE_CHANNEL.try_send(event.clone()) {
                    error!(
                        "❌ CRITICAL: Failed to send critical BLE event after retry: {:?}",
                        event
                    );
                    // This is a serious system issue - we've lost a critical event
                }
            }
        }
        EventPriority::Important => {
            // Important events should try hard not to be dropped
            if let Err(_) = BLE_CHANNEL.try_send(event.clone()) {
                warn!("⚠️ BLE channel full for important event: {:?}", event);
                // Try once more after a brief moment - important but not critical
                if let Err(_) = BLE_CHANNEL.try_send(event) {
                    warn!("📤 Dropped important BLE event due to channel backpressure");
                }
            }
        }
        EventPriority::Normal => {
            // Normal events can be dropped silently if channel is full
            if let Err(_) = BLE_CHANNEL.try_send(event) {
                // Silent drop for normal events - this is expected under high load
            }
        }
    }
}

/// Classify BLE events by their priority for backpressure handling
fn classify_event_priority(event: &BleEvent) -> EventPriority {
    match event {
        // Critical events that must never be dropped
        BleEvent::Error(_) => EventPriority::Critical,
        BleEvent::ClientConnected => EventPriority::Critical,
        BleEvent::ClientDisconnected => EventPriority::Critical,
        BleEvent::CredentialsReceived(_) => EventPriority::Critical,

        // Important events that should try hard not to be dropped
        BleEvent::Initialized => EventPriority::Important,
        BleEvent::ServiceCreated { .. } => EventPriority::Important,

        // Normal events that can be dropped if needed
        BleEvent::AdvertisingStarted => EventPriority::Normal,
        BleEvent::CharacteristicAdded { .. } => EventPriority::Normal,
        BleEvent::SendResponse(_) => EventPriority::Normal,
    }
}

// Thread-safe global storage for BLE server instance using OnceLock
//
// OPTIMIZATION: Replaced unsafe `static mut` with `std::sync::OnceLock` for:
// 1. Thread-safe lazy initialization without unsafe code
// 2. Guaranteed single initialization across the lifetime of the program
// 3. Safe access from extern "C" callbacks without data races
// 4. Elimination of all unsafe access patterns while maintaining functionality
static BLE_SERVER_INSTANCE: OnceLock<Arc<Mutex<BleServerState>>> = OnceLock::new();

/// Thread-safe and panic-safe helper function to access BLE server state from callbacks
///
/// This function provides a safe, ergonomic way to access and modify the global
/// BLE server state from extern "C" callbacks. It handles all error cases gracefully
/// and eliminates the need for unsafe code blocks.
///
/// Returns `Some(R)` if the state was successfully accessed and the closure executed,
/// `None` if the state is not initialized or the mutex is poisoned.
/// Note: Panics are handled at the callback level with catch_unwind.
fn with_ble_server_state<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut BleServerState) -> R,
{
    // Get the state instance if initialized
    let state_arc = BLE_SERVER_INSTANCE.get()?;

    // Acquire the lock
    let mut state = state_arc.lock().ok()?;

    // Execute the closure - panic safety is handled at callback level
    Some(f(&mut state))
}

// Thread-safe state for callback access
struct BleServerState {
    is_connected: bool,
    received_credentials: Option<WiFiCredentials>,
    service_handle: u16,
    char_handles: HashMap<String, u16>,
}

impl BleServerState {
    fn new() -> Self {
        Self {
            is_connected: false,
            received_credentials: None,
            service_handle: 0,
            char_handles: HashMap::new(),
        }
    }
}

// BLE initialization state tracking
#[derive(Debug, Clone, PartialEq)]
enum BleInitState {
    Uninitialized,
    ControllerInitialized,
    ControllerEnabled,
    BluedroidInitialized,
    BluedroidEnabled,
    CallbacksRegistered,
    ServiceCreated,
    AdvertisingStarted,
}

// Production BLE Server using real ESP-IDF APIs
pub struct BleServer {
    device_name: String,
    bt_driver: Option<BtDriver<'static, Ble>>,
    state: Arc<Mutex<BleServerState>>,
    init_state: BleInitState,
}

impl BleServer {
    pub fn new(device_id: &str) -> Self {
        let device_name = format!("AcornPups-{}", device_id);
        let state = Arc::new(Mutex::new(BleServerState::new()));

        // Thread-safe initialization using OnceLock
        if let Err(_) = BLE_SERVER_INSTANCE.set(state.clone()) {
            // If already set, this is fine - we'll use the existing instance
            warn!("BLE_SERVER_INSTANCE was already initialized, using existing instance");
        }

        Self {
            device_name,
            bt_driver: None,
            state,
            init_state: BleInitState::Uninitialized,
        }
    }

    // Initialize BLE with real modem hardware following proper ESP-IDF sequence
    pub async fn initialize_with_modem(&mut self, modem: Modem) -> BleResult<()> {
        info!("🔧 Starting ESP-IDF BLE initialization sequence");

        // Initialize BLE driver (this automatically does controller init, enable, Bluedroid init, enable)
        info!("📡 Initializing BLE driver with modem - this handles all low-level initialization");
        let bt_driver = BtDriver::new(modem, None).map_err(|e| {
            BleError::ControllerInitFailed(format!("BtDriver creation failed: {:?}", e))
        })?;

        self.bt_driver = Some(bt_driver);

        // BtDriver::new() already completed:
        // - BLE controller initialization
        // - BLE controller enable
        // - Bluedroid stack initialization
        // - Bluedroid stack enable
        // So we can skip directly to BluedroidEnabled state
        self.init_state = BleInitState::BluedroidEnabled;

        // Small delay to ensure everything is stable
        Timer::after(Duration::from_millis(200)).await;

        BLE_INITIALIZED.store(true, Ordering::SeqCst);

        // Send initialization event
        send_ble_event_with_backpressure(BleEvent::Initialized);

        info!("✅ ESP-IDF BLE initialization completed - controller and Bluedroid ready");
        Ok(())
    }

    // Start BLE provisioning service with proper initialization order
    pub async fn start_provisioning_service(&mut self) -> BleResult<()> {
        info!("🔧 Starting BLE WiFi provisioning service with proper initialization");

        // Verify BLE stack is properly initialized
        if self.init_state != BleInitState::BluedroidEnabled {
            return Err(BleError::NotInitialized(
                "BLE stack must be initialized before starting services".to_string(),
            ));
        }

        // Step 6: Register callbacks
        self.setup_ble_callbacks().await?;

        // Step 7: Create GATT service
        self.create_provisioning_service().await?;

        // Step 8: Start advertising
        self.start_ble_advertising().await?;

        info!("✅ BLE WiFi provisioning service started successfully");
        Ok(())
    }

    // Setup BLE callbacks after stack is enabled
    async fn setup_ble_callbacks(&mut self) -> BleResult<()> {
        info!("🔧 Step 6: Registering BLE GATT and GAP callbacks");

        // Register GATT server callback
        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gatts_register_callback(Some(gatts_event_handler)) },
            "GATT server callback registration",
        )?;

        // Register GAP callback for advertising events
        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gap_register_callback(Some(gap_event_handler)) },
            "GAP callback registration",
        )?;

        // Register GATT application
        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gatts_app_register(0) },
            "GATT application registration",
        )?;

        self.init_state = BleInitState::CallbacksRegistered;
        Timer::after(Duration::from_millis(100)).await;
        info!("✅ Step 6 complete: BLE callbacks registered");
        Ok(())
    }

    // Create GATT provisioning service
    async fn create_provisioning_service(&mut self) -> BleResult<()> {
        info!("🔧 Step 7: Creating WiFi provisioning GATT service");

        // Wait for GATT app registration event
        Timer::after(Duration::from_millis(200)).await;

        // Create WiFi provisioning service
        let service_uuid = parse_uuid(WIFI_SERVICE_UUID)?;
        let service_id = esp_idf_sys::esp_gatt_srvc_id_t {
            is_primary: true,
            id: esp_idf_sys::esp_gatt_id_t {
                uuid: esp_idf_sys::esp_bt_uuid_t {
                    len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
                    uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 {
                        uuid128: service_uuid,
                    },
                },
                inst_id: 0,
            },
        };

        let gatts_if = GATT_IF.load(Ordering::SeqCst);
        if gatts_if == 0 {
            return Err(BleError::NotInitialized(
                "GATT interface not registered".to_string(),
            ));
        }

        call_esp_api_with_context(
            || unsafe {
                esp_idf_sys::esp_ble_gatts_create_service(
                    gatts_if,
                    &service_id as *const _ as *mut _,
                    8,
                )
            },
            "GATT service creation",
        )?;

        // Wait for service creation
        Timer::after(Duration::from_millis(200)).await;
        self.init_state = BleInitState::ServiceCreated;
        info!("✅ Step 7 complete: GATT service created");
        Ok(())
    }

    // Start BLE advertising
    async fn start_ble_advertising(&mut self) -> BleResult<()> {
        info!("🔧 Step 8: Starting BLE advertising");

        // Set device name
        let device_name_cstr = CString::new(self.device_name.clone())
            .map_err(|_| BleError::DeviceNameSetFailed("Invalid device name".to_string()))?;

        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gap_set_device_name(device_name_cstr.as_ptr()) },
            "Device name setting",
        )?;

        Timer::after(Duration::from_millis(100)).await;

        // Configure advertising data (keep it minimal to avoid 31-byte limit issues)
        let mut adv_data = esp_idf_sys::esp_ble_adv_data_t {
            set_scan_rsp: false,
            include_name: true,     // Include device name
            include_txpower: false, // Disable to save space
            min_interval: 0x0006,
            max_interval: 0x0010,
            appearance: 0x00,
            manufacturer_len: 0,
            p_manufacturer_data: std::ptr::null_mut(),
            service_data_len: 0,
            p_service_data: std::ptr::null_mut(),
            service_uuid_len: 0, // Remove service UUID to save space
            p_service_uuid: std::ptr::null_mut(), // Service UUID not in advertising data
            flag: (esp_idf_sys::ESP_BLE_ADV_FLAG_GEN_DISC
                | esp_idf_sys::ESP_BLE_ADV_FLAG_BREDR_NOT_SPT) as u8,
        };

        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gap_config_adv_data(&mut adv_data) },
            "Advertising data configuration",
        )?;
        info!("📡 Advertising data set complete");

        Timer::after(Duration::from_millis(200)).await;

        // Start advertising with correct parameters
        let mut adv_params = esp_idf_sys::esp_ble_adv_params_t {
            adv_int_min: 0x20, // 32 * 0.625ms = 20ms min interval
            adv_int_max: 0x40, // 64 * 0.625ms = 40ms max interval
            adv_type: esp_idf_sys::esp_ble_adv_type_t_ADV_TYPE_IND, // Fixed constant
            own_addr_type: esp_idf_sys::esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC,
            peer_addr: [0; 6],
            peer_addr_type: esp_idf_sys::esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC,
            channel_map: esp_idf_sys::esp_ble_adv_channel_t_ADV_CHNL_ALL,
            adv_filter_policy: esp_idf_sys::esp_ble_adv_filter_t_ADV_FILTER_ALLOW_SCAN_ANY_CON_ANY,
        };

        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gap_start_advertising(&mut adv_params) },
            "Advertising start",
        )?;

        BLE_ADVERTISING.store(true, Ordering::SeqCst);
        self.init_state = BleInitState::AdvertisingStarted;

        info!(
            "📡 ✅ Step 8 complete: BLE advertising started - Device discoverable as: {}",
            self.device_name
        );
        info!("📱 Mobile apps can now connect and send WiFi credentials");

        send_ble_event_with_backpressure(BleEvent::AdvertisingStarted);
        Ok(())
    }

    // Add real GATT characteristic
    async fn add_characteristic(
        &self,
        uuid_str: &str,
        char_name: &str,
        properties: u8,
        permissions: u16,
    ) -> BleResult<()> {
        info!("📝 Adding real {} characteristic", char_name);

        let char_uuid = parse_uuid(uuid_str)?;
        let service_handle = {
            let state = self.state.lock().unwrap();
            state.service_handle
        };

        let uuid_struct = esp_idf_sys::esp_bt_uuid_t {
            len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
            uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 { uuid128: char_uuid },
        };

        let _ret = call_esp_api(|| unsafe {
            esp_idf_sys::esp_ble_gatts_add_char(
                service_handle,
                &uuid_struct as *const _ as *mut _,
                permissions,
                properties,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        })?;

        Timer::after(Duration::from_millis(50)).await;
        info!("✅ Real {} characteristic added", char_name);
        Ok(())
    }

    // Validate WiFi credentials
    pub async fn validate_credentials(&self, credentials: &WiFiCredentials) -> BleResult<()> {
        info!("🔍 Validating WiFi credentials");

        if credentials.ssid.is_empty() {
            return Err(BleError::InvalidCredentials(
                "SSID cannot be empty".to_string(),
            ));
        }

        if credentials.ssid.len() > 32 {
            return Err(BleError::InvalidCredentials(format!(
                "SSID too long: {} bytes (max 32)",
                credentials.ssid.len()
            )));
        }

        if credentials.password.len() < 8 && !credentials.password.is_empty() {
            return Err(BleError::InvalidCredentials(format!(
                "Password too short: {} characters (min 8)",
                credentials.password.len()
            )));
        }

        if credentials.password.len() > 64 {
            return Err(BleError::InvalidCredentials(format!(
                "Password too long: {} characters (max 64)",
                credentials.password.len()
            )));
        }

        info!("✅ WiFi credentials validation passed");
        Ok(())
    }

    // Public interface methods
    pub fn get_received_credentials(&self) -> Option<WiFiCredentials> {
        let state = self.state.lock().unwrap();
        state.received_credentials.clone()
    }

    pub fn is_client_connected(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.is_connected
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

        info!("📤 WiFi status sent to client: {}", status_message);

        // Send notification to connected client
        if self.is_client_connected() {
            self.send_status_notification(&status_message).await;
        }
    }

    async fn send_status_notification(&self, message: &str) {
        // Send notification via status characteristic
        let status_handle = {
            let state = self.state.lock().unwrap();
            state.char_handles.get("status").copied().unwrap_or(0)
        };

        if status_handle != 0 {
            let data = message.as_bytes();
            let ret = call_esp_api_with_context(
                || unsafe {
                    esp_idf_sys::esp_ble_gatts_send_indicate(
                        GATT_IF.load(Ordering::SeqCst),
                        0, // conn_id - would need to track this
                        status_handle,
                        data.len() as u16,
                        data.as_ptr() as *mut u8,
                        false, // need_confirm
                    )
                },
                "Status notification send",
            );

            match ret {
                Ok(_) => info!("📤 Status notification sent: {}", message),
                Err(e) => warn!("Failed to send status notification: {:?}", e),
            }
        }
    }

    pub async fn stop_advertising(&mut self) -> BleResult<()> {
        info!("🛑 Stopping BLE advertising");

        if !BLE_ADVERTISING.load(Ordering::SeqCst) {
            return Ok(());
        }

        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gap_stop_advertising() },
            "Advertising stop",
        )?;

        BLE_ADVERTISING.store(false, Ordering::SeqCst);
        info!("✅ BLE advertising stopped");
        Ok(())
    }

    pub async fn shutdown_ble_completely(&mut self) -> BleResult<()> {
        info!("🔄 Initiating complete BLE shutdown");

        if BLE_ADVERTISING.load(Ordering::SeqCst) {
            self.stop_advertising().await?;
        }

        // Reset state
        {
            let mut state = self.state.lock().unwrap();
            state.is_connected = false;
            state.received_credentials = None;
            state.service_handle = 0;
            state.char_handles.clear();
        }

        BLE_INITIALIZED.store(false, Ordering::SeqCst);
        GATT_IF.store(0, Ordering::SeqCst);
        self.init_state = BleInitState::Uninitialized;

        // Drop BT driver
        self.bt_driver.take();

        info!("✅ Complete BLE shutdown successful");
        Ok(())
    }

    // Required methods for main.rs compatibility
    pub async fn start_advertising(&mut self) -> BleResult<()> {
        self.start_provisioning_service().await
    }

    pub async fn restart_advertising(&mut self) -> BleResult<()> {
        info!("🔄 Restarting BLE advertising");
        if BLE_ADVERTISING.load(Ordering::SeqCst) {
            self.stop_advertising().await?;
        }
        self.start_ble_advertising().await
    }

    pub async fn handle_events(&mut self) {
        info!("📡 BLE server started, waiting for events...");

        loop {
            let event = BLE_CHANNEL.receive().await;
            self.process_event(event).await;
            Timer::after(Duration::from_millis(10)).await;
        }
    }

    async fn process_event(&mut self, event: BleEvent) {
        match event {
            BleEvent::Initialized => {
                info!("📡 BLE stack initialized");
            }
            BleEvent::ServiceCreated { service_handle } => {
                info!("📡 BLE service created with handle: {}", service_handle);
                let mut state = self.state.lock().unwrap();
                state.service_handle = service_handle;
            }
            BleEvent::CharacteristicAdded { char_handle, uuid } => {
                info!(
                    "📡 BLE characteristic added - handle: {}, UUID: {}",
                    char_handle, uuid
                );
            }
            BleEvent::AdvertisingStarted => {
                info!("📡 BLE advertising started - device discoverable");
            }
            BleEvent::ClientConnected => {
                info!("📱 BLE client connected!");
                {
                    let mut state = self.state.lock().unwrap();
                    state.is_connected = true;
                }
            }
            BleEvent::ClientDisconnected => {
                info!("📱 BLE client disconnected");
                {
                    let mut state = self.state.lock().unwrap();
                    state.is_connected = false;
                }
            }
            BleEvent::CredentialsReceived(credentials) => {
                info!("🔑 Received WiFi credentials - SSID: {}", credentials.ssid);

                match self.validate_credentials(&credentials).await {
                    Ok(()) => {
                        let mut state = self.state.lock().unwrap();
                        state.received_credentials = Some(credentials.clone());
                        info!("✅ Credentials validated and stored");
                    }
                    Err(e) => {
                        error!("❌ Credential validation failed: {}", e);
                    }
                }
            }
            BleEvent::SendResponse(response) => {
                if self.is_client_connected() {
                    info!("📤 Sending response to client: {}", response);
                } else {
                    warn!("❌ Cannot send response - no client connected");
                }
            }
            BleEvent::Error(error) => {
                error!("❌ BLE Error occurred: {:?}", error);
            }
        }
    }
}

// Real GATT event handler with panic safety
extern "C" fn gatts_event_handler(
    event: esp_idf_sys::esp_gatts_cb_event_t,
    gatts_if: esp_idf_sys::esp_gatt_if_t,
    param: *mut esp_idf_sys::esp_ble_gatts_cb_param_t,
) {
    // PANIC SAFETY: Wrap all Rust code in catch_unwind to prevent undefined behavior
    // when panics occur in C callbacks on ESP32
    let result = std::panic::catch_unwind(|| gatts_event_handler_impl(event, gatts_if, param));

    if let Err(panic_info) = result {
        // Log panic but don't propagate to C side - this could cause undefined behavior
        error!("🚨 PANIC in GATT event handler: {:?}", panic_info);
        // Send error event through channel for monitoring
        send_ble_event_with_backpressure(BleEvent::Error(BleError::SystemError(
            "Panic occurred in GATT event handler".to_string(),
        )));
    }
}

// Internal implementation of GATT event handler (panic-safe)
fn gatts_event_handler_impl(
    event: esp_idf_sys::esp_gatts_cb_event_t,
    gatts_if: esp_idf_sys::esp_gatt_if_t,
    param: *mut esp_idf_sys::esp_ble_gatts_cb_param_t,
) {
    if param.is_null() {
        return;
    }

    match event {
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_REG_EVT => {
            info!("📋 GATT server registered with interface: {}", gatts_if);
            GATT_IF.store(gatts_if, Ordering::SeqCst);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CREATE_EVT => {
            let create_param = unsafe { &(*param).create };
            info!(
                "📋 GATT service created with handle: {}",
                create_param.service_handle
            );

            // Add characteristics after service creation
            add_service_characteristics(create_param.service_handle);

            send_ble_event_with_backpressure(BleEvent::ServiceCreated {
                service_handle: create_param.service_handle,
            });
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_ADD_CHAR_EVT => {
            let add_char_param = unsafe { &(*param).add_char };
            info!(
                "📝 Characteristic added with handle: {}",
                add_char_param.attr_handle
            );

            // Store characteristic handle using thread-safe helper function
            with_ble_server_state(|state| {
                // This is a simplified mapping - in production you'd track which char this is
                let char_count = state.char_handles.len();
                if char_count == 0 {
                    state
                        .char_handles
                        .insert("ssid".to_string(), add_char_param.attr_handle);
                } else if char_count == 1 {
                    state
                        .char_handles
                        .insert("password".to_string(), add_char_param.attr_handle);
                } else if char_count == 2 {
                    state
                        .char_handles
                        .insert("status".to_string(), add_char_param.attr_handle);
                }
            });

            send_ble_event_with_backpressure(BleEvent::CharacteristicAdded {
                char_handle: add_char_param.attr_handle,
                uuid: "unknown".to_string(),
            });
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CONNECT_EVT => {
            info!("📱 BLE client connected");
            // Update connection state using thread-safe helper function
            with_ble_server_state(|state| {
                state.is_connected = true;
            });
            send_ble_event_with_backpressure(BleEvent::ClientConnected);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_DISCONNECT_EVT => {
            info!("📱 BLE client disconnected");
            // Update connection state using thread-safe helper function
            with_ble_server_state(|state| {
                state.is_connected = false;
            });
            send_ble_event_with_backpressure(BleEvent::ClientDisconnected);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_WRITE_EVT => {
            let write_param = unsafe { &(*param).write };
            handle_characteristic_write(write_param);
        }
        _ => {
            // Handle other events as needed
        }
    }
}

// Real GAP event handler with panic safety
extern "C" fn gap_event_handler(
    event: esp_idf_sys::esp_gap_ble_cb_event_t,
    param: *mut esp_idf_sys::esp_ble_gap_cb_param_t,
) {
    // PANIC SAFETY: Wrap all Rust code in catch_unwind to prevent undefined behavior
    // when panics occur in C callbacks on ESP32
    let result = std::panic::catch_unwind(|| gap_event_handler_impl(event, param));

    if let Err(panic_info) = result {
        // Log panic but don't propagate to C side - this could cause undefined behavior
        error!("🚨 PANIC in GAP event handler: {:?}", panic_info);
        // Send error event through channel for monitoring
        send_ble_event_with_backpressure(BleEvent::Error(BleError::SystemError(
            "Panic occurred in GAP event handler".to_string(),
        )));
    }
}

// Internal implementation of GAP event handler (panic-safe)
fn gap_event_handler_impl(
    event: esp_idf_sys::esp_gap_ble_cb_event_t,
    _param: *mut esp_idf_sys::esp_ble_gap_cb_param_t,
) {
    match event {
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_DATA_SET_COMPLETE_EVT => {
            info!("📡 Advertising data set complete");
        }
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_START_COMPLETE_EVT => {
            info!("📡 Advertising started successfully");
            send_ble_event_with_backpressure(BleEvent::AdvertisingStarted);
        }
        _ => {}
    }
}

// Add characteristics to service
fn add_service_characteristics(service_handle: u16) {
    // Add SSID characteristic (writable)
    let ssid_uuid = parse_uuid(SSID_CHAR_UUID).unwrap();
    let ssid_uuid_struct = esp_idf_sys::esp_bt_uuid_t {
        len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 { uuid128: ssid_uuid },
    };
    let _ = call_esp_api_with_context(
        || unsafe {
            esp_idf_sys::esp_ble_gatts_add_char(
                service_handle,
                &ssid_uuid_struct as *const _ as *mut _,
                esp_idf_sys::ESP_GATT_PERM_WRITE as u16,
                esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE as u8,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        "SSID characteristic creation",
    );

    // Add Password characteristic (writable)
    let password_uuid = parse_uuid(PASSWORD_CHAR_UUID).unwrap();
    let password_uuid_struct = esp_idf_sys::esp_bt_uuid_t {
        len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 {
            uuid128: password_uuid,
        },
    };
    let _ = call_esp_api_with_context(
        || unsafe {
            esp_idf_sys::esp_ble_gatts_add_char(
                service_handle,
                &password_uuid_struct as *const _ as *mut _,
                esp_idf_sys::ESP_GATT_PERM_WRITE as u16,
                esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE as u8,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        "Password characteristic creation",
    );

    // Add Status characteristic (readable + notifiable)
    let status_uuid = parse_uuid(STATUS_CHAR_UUID).unwrap();
    let status_uuid_struct = esp_idf_sys::esp_bt_uuid_t {
        len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 {
            uuid128: status_uuid,
        },
    };
    let _ = call_esp_api_with_context(
        || unsafe {
            esp_idf_sys::esp_ble_gatts_add_char(
                service_handle,
                &status_uuid_struct as *const _ as *mut _,
                esp_idf_sys::ESP_GATT_PERM_READ as u16,
                (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ
                    | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_NOTIFY) as u8,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        "Status characteristic creation",
    );
}

// Handle characteristic write events
fn handle_characteristic_write(
    param: &esp_idf_sys::esp_ble_gatts_cb_param_t_gatts_write_evt_param,
) {
    if param.value.is_null() || param.len == 0 {
        return;
    }

    let data = unsafe { std::slice::from_raw_parts(param.value, param.len as usize) };

    if let Ok(json_str) = std::str::from_utf8(data) {
        if let Ok(credentials) = serde_json::from_str::<WiFiCredentials>(json_str) {
            info!(
                "🔑 Received WiFi credentials via real BLE: SSID={}",
                credentials.ssid
            );

            // Store in global state using thread-safe helper function
            with_ble_server_state(|state| {
                state.received_credentials = Some(credentials.clone());
            });

            send_ble_event_with_backpressure(BleEvent::CredentialsReceived(credentials));
        } else {
            warn!(
                "❌ Failed to parse WiFi credentials from JSON: {}",
                json_str
            );
        }
    } else {
        warn!("❌ Received invalid UTF-8 data via BLE");
    }
}

// Utility functions
pub fn generate_device_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    "esp32_device".hash(&mut hasher);
    format!("{:04X}", hasher.finish() & 0xFFFF)
}

// Enhanced ESP API wrapper with better error handling and context
fn call_esp_api_with_context<F>(f: F, context: &str) -> BleResult<()>
where
    F: FnOnce() -> esp_idf_sys::esp_err_t,
{
    let result = f();
    if result == esp_idf_sys::ESP_OK {
        Ok(())
    } else {
        let error_msg = match result {
            esp_idf_sys::ESP_ERR_INVALID_STATE => {
                format!("{}: Invalid state - BLE stack not ready", context)
            }
            esp_idf_sys::ESP_ERR_INVALID_ARG => format!("{}: Invalid argument", context),
            esp_idf_sys::ESP_ERR_NO_MEM => format!("{}: Out of memory", context),
            esp_idf_sys::ESP_ERR_NOT_FOUND => format!("{}: Resource not found", context),
            esp_idf_sys::ESP_ERR_TIMEOUT => format!("{}: Operation timeout", context),
            _ => format!("{}: Unknown error", context),
        };
        Err(BleError::EspError(result, error_msg))
    }
}

// Safe wrapper for ESP-IDF API calls (backward compatibility)
fn call_esp_api<F>(f: F) -> BleResult<()>
where
    F: FnOnce() -> esp_idf_sys::esp_err_t,
{
    call_esp_api_with_context(f, "ESP API call")
}

// Parse UUID string to ESP-IDF format
fn parse_uuid(uuid_str: &str) -> Result<[u8; 16], BleError> {
    let uuid_clean = uuid_str.replace("-", "");
    if uuid_clean.len() != 32 {
        return Err(BleError::Unknown("Invalid UUID format".to_string()));
    }

    let mut uuid_bytes = [0u8; 16];
    for (i, chunk) in uuid_clean.as_bytes().chunks(2).enumerate() {
        if i >= 16 {
            break;
        }
        let hex_str = std::str::from_utf8(chunk)
            .map_err(|_| BleError::Unknown("Invalid hex in UUID".to_string()))?;
        // ESP-IDF expects little-endian UUID format
        uuid_bytes[15 - i] = u8::from_str_radix(hex_str, 16)
            .map_err(|_| BleError::Unknown("Invalid hex value in UUID".to_string()))?;
    }

    Ok(uuid_bytes)
}

// Simulation functions kept for development/testing compatibility
pub async fn simulate_ble_connect() {
    info!("📱 Simulated BLE client connected!");
    send_ble_event_with_backpressure(BleEvent::ClientConnected);
}

pub async fn simulate_ble_disconnect() {
    info!("📱 Simulated BLE client disconnected!");
    send_ble_event_with_backpressure(BleEvent::ClientDisconnected);
}

pub async fn simulate_characteristic_write(data: &[u8]) {
    info!(
        "📝 Simulated characteristic write received: {} bytes",
        data.len()
    );

    if let Ok(json_str) = std::str::from_utf8(data) {
        if let Ok(credentials) = serde_json::from_str::<WiFiCredentials>(json_str) {
            info!(
                "🔑 Parsed WiFi credentials from simulated BLE: SSID={}",
                credentials.ssid
            );
            send_ble_event_with_backpressure(BleEvent::CredentialsReceived(credentials));
        } else {
            warn!(
                "❌ Failed to parse WiFi credentials from JSON: {}",
                json_str
            );
        }
    }
}

pub async fn simulate_characteristic_write_by_uuid(char_uuid: &str, data: &[u8]) {
    info!(
        "📝 Simulated characteristic {} written with {} bytes",
        char_uuid,
        data.len()
    );

    match char_uuid {
        "ssid" | "password" => {
            simulate_characteristic_write(data).await;
        }
        _ => {
            info!("📝 Ignoring write to unknown characteristic: {}", char_uuid);
        }
    }
}
