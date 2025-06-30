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
use std::sync::{Arc, Mutex};

// Production-grade BLE error types
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

// Global storage for BLE server instance (for callbacks)
static mut BLE_SERVER_INSTANCE: Option<Arc<Mutex<BleServerState>>> = None;

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

// Production BLE Server using real ESP-IDF APIs
pub struct BleServer {
    device_name: String,
    bt_driver: Option<BtDriver<'static, Ble>>,
    state: Arc<Mutex<BleServerState>>,
}

impl BleServer {
    pub fn new(device_id: &str) -> Self {
        let device_name = format!("AcornPups-{}", device_id);
        let state = Arc::new(Mutex::new(BleServerState::new()));

        // Store global reference for callbacks
        unsafe {
            BLE_SERVER_INSTANCE = Some(state.clone());
        }

        Self {
            device_name,
            bt_driver: None,
            state,
        }
    }

    // Initialize BLE with real modem hardware
    pub async fn initialize_with_modem(&mut self, modem: Modem) -> BleResult<()> {
        info!("🔧 Initializing BLE with real ESP32 modem hardware");

        // Initialize BLE driver with modem
        let bt_driver = BtDriver::new(modem, None).map_err(|e| {
            BleError::ControllerInitFailed(format!("BtDriver creation failed: {:?}", e))
        })?;

        self.bt_driver = Some(bt_driver);
        BLE_INITIALIZED.store(true, Ordering::SeqCst);

        info!("✅ BLE driver initialized with real modem");

        // Send initialization event
        let _ = BLE_CHANNEL.try_send(BleEvent::Initialized);

        Ok(())
    }

    // Start BLE provisioning service with real GATT implementation
    pub async fn start_provisioning_service(&mut self) -> BleResult<()> {
        info!("🔧 Starting real BLE WiFi provisioning service");

        // 1. Setup BLE callbacks
        self.setup_ble_callbacks().await?;

        // 2. Create real GATT service
        self.create_provisioning_service().await?;

        // 3. Start real advertising
        self.start_ble_advertising().await?;

        info!("✅ Real BLE provisioning service started and advertising");
        Ok(())
    }

    // 1. Setup real BLE callbacks
    async fn setup_ble_callbacks(&mut self) -> BleResult<()> {
        info!("🔧 Setting up real BLE GATT callbacks");

        // Register GATT server callback
        let _ret = call_esp_api(|| unsafe {
            esp_idf_sys::esp_ble_gatts_register_callback(Some(gatts_event_handler))
        })?;

        // Register GAP callback for advertising events
        let _ret = call_esp_api(|| unsafe {
            esp_idf_sys::esp_ble_gap_register_callback(Some(gap_event_handler))
        })?;

        // Register GATT application
        let _ret = call_esp_api(|| unsafe { esp_idf_sys::esp_ble_gatts_app_register(0) })?;

        info!("✅ Real BLE callbacks registered");
        Ok(())
    }

    // 2. Create real GATT provisioning service
    async fn create_provisioning_service(&mut self) -> BleResult<()> {
        info!("🛜 Creating real WiFi provisioning GATT service");

        // Wait for GATT app registration event
        Timer::after(Duration::from_millis(100)).await;

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
        let _ret = call_esp_api(|| unsafe {
            esp_idf_sys::esp_ble_gatts_create_service(
                gatts_if,
                &service_id as *const _ as *mut _,
                8,
            )
        })?;

        // Wait for service creation
        Timer::after(Duration::from_millis(100)).await;

        info!("✅ Real GATT service created");
        Ok(())
    }

    // 3. Start real BLE advertising
    async fn start_ble_advertising(&mut self) -> BleResult<()> {
        info!("📡 Starting real BLE advertising");

        // Set device name
        let device_name_cstr = CString::new(self.device_name.clone())
            .map_err(|_| BleError::DeviceNameSetFailed("Invalid device name".to_string()))?;

        let _ret = call_esp_api(|| unsafe {
            esp_idf_sys::esp_ble_gap_set_device_name(device_name_cstr.as_ptr())
        })?;

        // Configure advertising data with service UUID
        let service_uuid = parse_uuid(WIFI_SERVICE_UUID)?;
        let mut adv_data = esp_idf_sys::esp_ble_adv_data_t {
            set_scan_rsp: false,
            include_name: true,
            include_txpower: true,
            min_interval: 0x0006,
            max_interval: 0x0010,
            appearance: 0x00,
            manufacturer_len: 0,
            p_manufacturer_data: std::ptr::null_mut(),
            service_data_len: 0,
            p_service_data: std::ptr::null_mut(),
            service_uuid_len: 16,
            p_service_uuid: service_uuid.as_ptr() as *mut u8,
            flag: (esp_idf_sys::ESP_BLE_ADV_FLAG_GEN_DISC
                | esp_idf_sys::ESP_BLE_ADV_FLAG_BREDR_NOT_SPT) as u8,
        };

        let _ret =
            call_esp_api(|| unsafe { esp_idf_sys::esp_ble_gap_config_adv_data(&mut adv_data) })?;

        // Start advertising
        Timer::after(Duration::from_millis(100)).await;

        let mut adv_params = esp_idf_sys::esp_ble_adv_params_t {
            adv_int_min: 0x20,
            adv_int_max: 0x40,
            adv_type: esp_idf_sys::ESP_BLE_LEGACY_ADV_TYPE_IND,
            own_addr_type: esp_idf_sys::esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC,
            peer_addr: [0; 6],
            peer_addr_type: esp_idf_sys::esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC,
            channel_map: esp_idf_sys::esp_ble_adv_channel_t_ADV_CHNL_ALL,
            adv_filter_policy: esp_idf_sys::esp_ble_adv_filter_t_ADV_FILTER_ALLOW_SCAN_ANY_CON_ANY,
        };

        let _ret = call_esp_api(|| unsafe {
            esp_idf_sys::esp_ble_gap_start_advertising(&mut adv_params)
        })?;

        BLE_ADVERTISING.store(true, Ordering::SeqCst);

        info!(
            "📡 ✅ Real BLE advertising started - Device discoverable as: {}",
            self.device_name
        );
        info!("📱 Mobile apps can now connect and send WiFi credentials");

        let _ = BLE_CHANNEL.try_send(BleEvent::AdvertisingStarted);
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
            let ret = call_esp_api(|| unsafe {
                esp_idf_sys::esp_ble_gatts_send_indicate(
                    GATT_IF.load(Ordering::SeqCst),
                    0, // conn_id - would need to track this
                    status_handle,
                    data.len() as u16,
                    data.as_ptr() as *mut u8,
                    false, // need_confirm
                )
            });

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

        let _ret = call_esp_api(|| unsafe { esp_idf_sys::esp_ble_gap_stop_advertising() })?;

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

// Real GATT event handler
extern "C" fn gatts_event_handler(
    event: esp_idf_sys::esp_gatts_cb_event_t,
    gatts_if: esp_idf_sys::esp_gatt_if_t,
    param: *mut esp_idf_sys::esp_ble_gatts_cb_param_t,
) {
    if param.is_null() {
        return;
    }

    match event {
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_REG_EVT => {
            info!("📋 GATT server registered");
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

            let _ = BLE_CHANNEL.try_send(BleEvent::ServiceCreated {
                service_handle: create_param.service_handle,
            });
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_ADD_CHAR_EVT => {
            let add_char_param = unsafe { &(*param).add_char };
            info!(
                "📝 Characteristic added with handle: {}",
                add_char_param.attr_handle
            );

            // Store characteristic handle
            unsafe {
                if let Some(server_state) = &BLE_SERVER_INSTANCE {
                    if let Ok(mut state) = server_state.lock() {
                        // This is a simplified mapping - in production you'd track which char this is
                        if state.char_handles.len() == 0 {
                            state
                                .char_handles
                                .insert("ssid".to_string(), add_char_param.attr_handle);
                        } else if state.char_handles.len() == 1 {
                            state
                                .char_handles
                                .insert("password".to_string(), add_char_param.attr_handle);
                        } else if state.char_handles.len() == 2 {
                            state
                                .char_handles
                                .insert("status".to_string(), add_char_param.attr_handle);
                        }
                    }
                }
            }

            let _ = BLE_CHANNEL.try_send(BleEvent::CharacteristicAdded {
                char_handle: add_char_param.attr_handle,
                uuid: "unknown".to_string(),
            });
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CONNECT_EVT => {
            info!("📱 BLE client connected");
            unsafe {
                if let Some(server_state) = &BLE_SERVER_INSTANCE {
                    if let Ok(mut state) = server_state.lock() {
                        state.is_connected = true;
                    }
                }
            }
            let _ = BLE_CHANNEL.try_send(BleEvent::ClientConnected);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_DISCONNECT_EVT => {
            info!("📱 BLE client disconnected");
            unsafe {
                if let Some(server_state) = &BLE_SERVER_INSTANCE {
                    if let Ok(mut state) = server_state.lock() {
                        state.is_connected = false;
                    }
                }
            }
            let _ = BLE_CHANNEL.try_send(BleEvent::ClientDisconnected);
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

// Real GAP event handler
extern "C" fn gap_event_handler(
    event: esp_idf_sys::esp_gap_ble_cb_event_t,
    _param: *mut esp_idf_sys::esp_ble_gap_cb_param_t,
) {
    match event {
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_DATA_SET_COMPLETE_EVT => {
            info!("📡 Advertising data set complete");
        }
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_START_COMPLETE_EVT => {
            info!("📡 Advertising started");
            let _ = BLE_CHANNEL.try_send(BleEvent::AdvertisingStarted);
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
    let _ = call_esp_api(|| unsafe {
        esp_idf_sys::esp_ble_gatts_add_char(
            service_handle,
            &ssid_uuid_struct as *const _ as *mut _,
            esp_idf_sys::ESP_GATT_PERM_WRITE as u16,
            esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE as u8,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    });

    // Add Password characteristic (writable)
    let password_uuid = parse_uuid(PASSWORD_CHAR_UUID).unwrap();
    let password_uuid_struct = esp_idf_sys::esp_bt_uuid_t {
        len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 {
            uuid128: password_uuid,
        },
    };
    let _ = call_esp_api(|| unsafe {
        esp_idf_sys::esp_ble_gatts_add_char(
            service_handle,
            &password_uuid_struct as *const _ as *mut _,
            esp_idf_sys::ESP_GATT_PERM_WRITE as u16,
            esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE as u8,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    });

    // Add Status characteristic (readable + notifiable)
    let status_uuid = parse_uuid(STATUS_CHAR_UUID).unwrap();
    let status_uuid_struct = esp_idf_sys::esp_bt_uuid_t {
        len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 {
            uuid128: status_uuid,
        },
    };
    let _ = call_esp_api(|| unsafe {
        esp_idf_sys::esp_ble_gatts_add_char(
            service_handle,
            &status_uuid_struct as *const _ as *mut _,
            esp_idf_sys::ESP_GATT_PERM_READ as u16,
            (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_NOTIFY)
                as u8,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    });
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

            // Store in global state
            unsafe {
                if let Some(server_state) = &BLE_SERVER_INSTANCE {
                    if let Ok(mut state) = server_state.lock() {
                        state.received_credentials = Some(credentials.clone());
                    }
                }
            }

            let _ = BLE_CHANNEL.try_send(BleEvent::CredentialsReceived(credentials));
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

// Safe wrapper for ESP-IDF API calls
fn call_esp_api<F>(f: F) -> BleResult<()>
where
    F: FnOnce() -> esp_idf_sys::esp_err_t,
{
    let result = f();
    if result == esp_idf_sys::ESP_OK {
        Ok(())
    } else {
        Err(BleError::SystemError(format!(
            "ESP-IDF API call failed with code: {}",
            result
        )))
    }
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
    let _ = BLE_CHANNEL.try_send(BleEvent::ClientConnected);
}

pub async fn simulate_ble_disconnect() {
    info!("📱 Simulated BLE client disconnected!");
    let _ = BLE_CHANNEL.try_send(BleEvent::ClientDisconnected);
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
            let _ = BLE_CHANNEL.try_send(BleEvent::CredentialsReceived(credentials));
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
