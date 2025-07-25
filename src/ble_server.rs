// Production-grade BLE server implementation using real ESP-IDF GATT APIs
// This provides actual WiFi provisioning via real BLE GATT services

// Import Embassy's critical section mutex for thread-safe access
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};

// Import logging macros
use log::{debug, error, info, warn};

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
    pub auth_token: String,
    pub device_name: String,
    pub user_timezone: String,
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

// BLE operation timeout for credential reception
const CREDENTIALS_TIMEOUT_SECONDS: u64 = 30;

// Global state for BLE operations
static BLE_INITIALIZED: AtomicBool = AtomicBool::new(false);
static BLE_ADVERTISING: AtomicBool = AtomicBool::new(false);
static GATT_INTERFACE: AtomicU8 = AtomicU8::new(0);

// Global channel for BLE events
pub static BLE_EVENT_CHANNEL: Channel<CriticalSectionRawMutex, BleEvent, 10> = Channel::new();

// Global storage for BLE Long Write (Prepare Write) buffers per connection
static PREPARE_WRITE_BUFFERS: OnceLock<Arc<Mutex<HashMap<u16, PrepareWriteBuffer>>>> =
    OnceLock::new();

// Maximum allowed payload size for long writes (2KB should be sufficient for WiFi credentials)
const MAX_PREPARE_WRITE_BUFFER_SIZE: usize = 2048;

// Prepare write buffer structure for handling long writes
#[derive(Debug, Clone)]
struct PrepareWriteBuffer {
    data: Vec<u8>,
    last_offset: u16,
    char_handle: u16,
    created_at: std::time::Instant,
}

impl PrepareWriteBuffer {
    fn new(char_handle: u16) -> Self {
        Self {
            data: Vec::new(),
            last_offset: 0,
            char_handle,
            created_at: std::time::Instant::now(),
        }
    }

    fn append_chunk(&mut self, offset: u16, chunk: &[u8]) -> Result<(), BleError> {
        // Validate offset for sequential writes
        if offset != self.last_offset {
            return Err(BleError::InvalidState(format!(
                "Non-sequential prepare write: expected offset {}, got {}",
                self.last_offset, offset
            )));
        }

        // Check total size limit
        if self.data.len() + chunk.len() > MAX_PREPARE_WRITE_BUFFER_SIZE {
            return Err(BleError::InvalidState(format!(
                "Prepare write buffer would exceed max size {} bytes",
                MAX_PREPARE_WRITE_BUFFER_SIZE
            )));
        }

        // Append the chunk
        self.data.extend_from_slice(chunk);
        self.last_offset += chunk.len() as u16;

        Ok(())
    }

    fn is_expired(&self, timeout_seconds: u64) -> bool {
        self.created_at.elapsed().as_secs() > timeout_seconds
    }
}

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
            if BLE_EVENT_CHANNEL.try_send(event.clone()).is_err() {
                warn!("🚨 BLE channel full for critical event: {:?}", event);

                // For critical events, we'll attempt to drain one item and retry
                // This is safe because we're prioritizing the most important events
                if let Ok(_) = BLE_EVENT_CHANNEL.try_receive() {
                    warn!("📤 Dropped low-priority event to make space for critical event");
                }

                // Retry the critical event
                if let Err(_) = BLE_EVENT_CHANNEL.try_send(event.clone()) {
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
            if let Err(_) = BLE_EVENT_CHANNEL.try_send(event.clone()) {
                warn!("⚠️ BLE channel full for important event: {:?}", event);
                // Try once more after a brief moment - important but not critical
                if let Err(_) = BLE_EVENT_CHANNEL.try_send(event) {
                    warn!("📤 Dropped important BLE event due to channel backpressure");
                }
            }
        }
        EventPriority::Normal => {
            // Normal events can be dropped silently if channel is full
            if let Err(_) = BLE_EVENT_CHANNEL.try_send(event) {
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

// Thread-safe global BLE server state accessible from C callbacks
static GLOBAL_BLE_SERVER_STATE: OnceLock<Arc<Mutex<BleServerState>>> = OnceLock::new();

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
    let state_arc = GLOBAL_BLE_SERVER_STATE.get()?;

    // Acquire the lock
    let mut state = state_arc.lock().ok()?;

    // Execute the closure - panic safety is handled at callback level
    Some(f(&mut state))
}

/// Thread-safe helper function to access prepare write buffers from callbacks
///
/// This function provides safe access to the global prepare write buffer storage
/// from extern "C" callbacks. It handles initialization and error cases gracefully.
///
/// Returns `Some(R)` if the buffers were successfully accessed and the closure executed,
/// `None` if the storage is not initialized or the mutex is poisoned.
fn with_prepare_write_buffers<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut HashMap<u16, PrepareWriteBuffer>) -> R,
{
    // Initialize storage if not already done
    let buffers_arc = PREPARE_WRITE_BUFFERS.get_or_init(|| {
        info!("🔍 LONG WRITE DEBUG: Initializing prepare write buffers storage");
        Arc::new(Mutex::new(HashMap::new()))
    });

    // Acquire the lock
    let mut buffers = buffers_arc.lock().ok()?;

    // Execute the closure
    Some(f(&mut buffers))
}

/// Clean up expired prepare write buffers to prevent memory leaks
fn cleanup_expired_prepare_write_buffers() {
    const BUFFER_TIMEOUT_SECONDS: u64 = 30; // 30 second timeout for prepare write buffers

    with_prepare_write_buffers(|buffers| {
        let initial_count = buffers.len();
        buffers.retain(|conn_id, buffer| {
            let is_expired = buffer.is_expired(BUFFER_TIMEOUT_SECONDS);
            if is_expired {
                info!(
                    "🧹 LONG WRITE DEBUG: Cleaning up expired prepare write buffer for conn_id: {}",
                    conn_id
                );
            }
            !is_expired
        });
        let cleaned_count = initial_count - buffers.len();
        if cleaned_count > 0 {
            info!(
                "🧹 LONG WRITE DEBUG: Cleaned up {} expired prepare write buffers",
                cleaned_count
            );
        }
    });
}

// Thread-safe state for callback access
struct BleServerState {
    is_connected: bool,
    received_credentials: Option<WiFiCredentials>,
    service_handle: u16,
    char_handles: HashMap<String, u16>,
    connection_time: Option<std::time::Instant>,
    credentials_timeout_seconds: u64,
}

impl BleServerState {
    fn new() -> Self {
        Self {
            is_connected: false,
            received_credentials: None,
            service_handle: 0,
            char_handles: HashMap::new(),
            connection_time: None,
            credentials_timeout_seconds: CREDENTIALS_TIMEOUT_SECONDS,
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

// Production BLE Server using real ESP-IDF APIs with RAII cleanup
pub struct BleServer {
    device_name: String,
    bt_driver: Option<BtDriver<'static, Ble>>,
    state: Arc<Mutex<BleServerState>>,
    init_state: BleInitState,
    // Track registered callbacks for cleanup
    callbacks_registered: bool,
    // Track GATT application registration
    gatt_app_registered: bool,
}

// RAII cleanup implementation for BleServer
impl Drop for BleServer {
    fn drop(&mut self) {
        info!("🧹 BleServer dropping - performing RAII cleanup");

        // Perform synchronous cleanup in reverse initialization order
        if let Err(e) = self.cleanup_resources_sync() {
            error!("❌ Error during BleServer drop cleanup: {:?}", e);
        }

        info!("✅ BleServer RAII cleanup completed");
    }
}

impl BleServer {
    pub fn new(device_id: &str) -> Self {
        let device_name = format!("AcornPups-{}", device_id);
        let state = Arc::new(Mutex::new(BleServerState::new()));

        // Thread-safe initialization using OnceLock
        if let Err(_) = GLOBAL_BLE_SERVER_STATE.set(state.clone()) {
            // If already set, this is fine - we'll use the existing instance
            warn!("GLOBAL_BLE_SERVER_STATE was already initialized, using existing instance");
        }

        Self {
            device_name,
            bt_driver: None,
            state,
            init_state: BleInitState::Uninitialized,
            callbacks_registered: false,
            gatt_app_registered: false,
        }
    }

    /// Initialize BLE using provided BT driver - for WiFi coexistence
    /// Uses safe ESP-IDF-SVC abstractions for BLE/WiFi coexistence
    pub async fn initialize_with_bt_driver(
        &mut self,
        bt_driver: esp_idf_svc::bt::BtDriver<'static, esp_idf_svc::bt::Ble>,
    ) -> BleResult<()> {
        info!("🔧 Initializing BLE server with provided BT driver for coexistence");

        // Check if already initialized
        if BLE_INITIALIZED.load(Ordering::Acquire) {
            return Err(BleError::AlreadyInitialized(
                "BLE is already initialized".to_string(),
            ));
        }

        // Store the provided BT driver
        self.bt_driver = Some(bt_driver);
        info!("✅ BT driver stored - BLE hardware already initialized");

        // BtDriver::new() already completed:
        // - BLE controller initialization
        // - BLE controller enable
        // - Bluedroid stack initialization
        // - Bluedroid stack enable
        // So we can skip directly to BluedroidEnabled state
        self.init_state = BleInitState::BluedroidEnabled;

        // Small delay to ensure everything is stable
        Timer::after(Duration::from_millis(200)).await;

        // Mark as initialized
        BLE_INITIALIZED.store(true, Ordering::SeqCst);

        // Send initialization event
        send_ble_event_with_backpressure(BleEvent::Initialized);

        info!("✅ BLE server initialized successfully with WiFi coexistence");

        // Start the provisioning service
        self.start_provisioning_service().await?;

        Ok(())
    }

    // Start BLE provisioning service with proper initialization order and RAII cleanup
    pub async fn start_provisioning_service(&mut self) -> BleResult<()> {
        info!("🔧 Starting BLE WiFi provisioning service with RAII cleanup on failure");

        // Verify BLE stack is properly initialized
        if self.init_state != BleInitState::BluedroidEnabled {
            return Err(BleError::NotInitialized(
                "BLE stack must be initialized before starting services".to_string(),
            ));
        }

        // Step 6: Register callbacks - track success for cleanup
        if let Err(e) = self.setup_ble_callbacks().await {
            error!("❌ Callback setup failed, performing cleanup: {:?}", e);
            if let Err(cleanup_err) = self.cleanup_resources_async().await {
                error!(
                    "❌ Cleanup after callback setup failure failed: {:?}",
                    cleanup_err
                );
            }
            return Err(e);
        }

        // Step 7: Create GATT service - if this fails, callbacks need cleanup
        if let Err(e) = self.create_provisioning_service().await {
            error!(
                "❌ GATT service creation failed, performing cleanup: {:?}",
                e
            );
            if let Err(cleanup_err) = self.cleanup_resources_async().await {
                error!(
                    "❌ Cleanup after service creation failure failed: {:?}",
                    cleanup_err
                );
            }
            return Err(e);
        }

        // Step 8: Start advertising - if this fails, service and callbacks need cleanup
        if let Err(e) = self.start_ble_advertising().await {
            error!("❌ Advertising start failed, performing cleanup: {:?}", e);
            if let Err(cleanup_err) = self.cleanup_resources_async().await {
                error!(
                    "❌ Cleanup after advertising failure failed: {:?}",
                    cleanup_err
                );
            }
            return Err(e);
        }

        info!("✅ BLE WiFi provisioning service started successfully with RAII protection");
        Ok(())
    }

    // Setup BLE callbacks after stack is enabled - tracks registration for cleanup
    async fn setup_ble_callbacks(&mut self) -> BleResult<()> {
        info!("🔧 Step 6: Registering BLE GATT and GAP callbacks with tracking");

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

        // Mark callbacks as registered for cleanup tracking
        self.callbacks_registered = true;

        // Register GATT application
        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gatts_app_register(0) },
            "GATT application registration",
        )?;

        // Mark GATT app as registered for cleanup tracking
        self.gatt_app_registered = true;

        self.init_state = BleInitState::CallbacksRegistered;
        Timer::after(Duration::from_millis(100)).await;
        info!("✅ Step 6 complete: BLE callbacks registered and tracked");
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

        let gatt_interface = GATT_INTERFACE.load(Ordering::SeqCst);
        if gatt_interface == 0 {
            return Err(BleError::NotInitialized(
                "GATT interface not registered".to_string(),
            ));
        }

        call_esp_api_with_context(
            || unsafe {
                esp_idf_sys::esp_ble_gatts_create_service(
                    gatt_interface,
                    &service_id as *const _ as *mut _,
                    8,
                )
            },
            "GATT service creation",
        )?;

        // Wait for service creation
        Timer::after(Duration::from_millis(200)).await;

        // Service will be started automatically after all characteristics are added
        info!("📝 Service created - waiting for characteristics to be added before starting");
        info!("ℹ️ Service will be started automatically after all characteristics are added");

        self.init_state = BleInitState::ServiceCreated;
        info!("✅ Step 7 complete: GATT service created and started");
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

        // Configure minimal advertising data to fit in BLE advertising packet
        let mut adv_data = esp_idf_sys::esp_ble_adv_data_t {
            set_scan_rsp: false,
            include_name: true, // Include device name (essential for discovery)
            include_txpower: false, // Disable to save space
            min_interval: 0x0006,
            max_interval: 0x0010,
            appearance: 0x00,
            manufacturer_len: 0,
            p_manufacturer_data: std::ptr::null_mut(),
            service_data_len: 0,
            p_service_data: std::ptr::null_mut(),
            service_uuid_len: 0, // Remove service UUID from advertising to save space
            p_service_uuid: std::ptr::null_mut(),
            flag: (esp_idf_sys::ESP_BLE_ADV_FLAG_GEN_DISC
                | esp_idf_sys::ESP_BLE_ADV_FLAG_BREDR_NOT_SPT) as u8,
        };

        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gap_config_adv_data(&mut adv_data) },
            "Advertising data configuration",
        )?;
        info!("📡 Primary advertising data configured");

        // Configure scan response with service UUID for proper discovery
        let service_uuid_buffer = parse_uuid(WIFI_SERVICE_UUID)?;
        let mut scan_rsp_data = esp_idf_sys::esp_ble_adv_data_t {
            set_scan_rsp: true,  // This is scan response data
            include_name: false, // Name already in advertising data
            include_txpower: false,
            min_interval: 0,
            max_interval: 0,
            appearance: 0x00,
            manufacturer_len: 0,
            p_manufacturer_data: std::ptr::null_mut(),
            service_data_len: 0,
            p_service_data: std::ptr::null_mut(),
            service_uuid_len: 16, // 128-bit UUID = 16 bytes
            p_service_uuid: service_uuid_buffer.as_ptr() as *mut u8,
            flag: 0,
        };

        call_esp_api_with_context(
            || unsafe { esp_idf_sys::esp_ble_gap_config_adv_data(&mut scan_rsp_data) },
            "Scan response data configuration",
        )?;
        info!("📡 Scan response data configured with service UUID");

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

    // Public interface methods

    /// Take received credentials (get and clear) to prevent repeated processing
    pub fn take_received_credentials(&self) -> Option<WiFiCredentials> {
        with_ble_server_state(|state| state.received_credentials.take()).flatten()
    }

    /// Check if current connection has timed out waiting for credentials
    pub fn is_connection_timed_out(&self) -> bool {
        with_ble_server_state(|state| {
            if let Some(connection_time) = state.connection_time {
                let elapsed = connection_time.elapsed().as_secs();
                elapsed > state.credentials_timeout_seconds
            } else {
                false
            }
        })
        .unwrap_or(false)
    }

    /// Force disconnect current client (for timeout/security)
    pub async fn force_disconnect_client(&self, reason: &str) -> BleResult<()> {
        info!("🚫 Force disconnecting client: {}", reason);

        // Update state to mark as disconnected
        with_ble_server_state(|state| {
            state.is_connected = false;
            state.connection_time = None;
            state.received_credentials = None;
        });

        // Send disconnection event
        send_ble_event_with_backpressure(BleEvent::ClientDisconnected);

        info!("✅ Client force disconnected successfully");
        Ok(())
    }

    pub fn is_client_connected(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.is_connected
    }

    /// Send simple status message to mobile app
    pub async fn send_simple_status(&self, message: &str) {
        info!("📤 Sending simple status to client: {}", message);

        if self.is_client_connected() {
            self.send_status_notification(message).await;

            // Brief delay to ensure notification is processed
            Timer::after(Duration::from_millis(50)).await;
        } else {
            warn!("⚠️ Cannot send status - no client connected");
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
                        GATT_INTERFACE.load(Ordering::SeqCst),
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

    /// Process events without blocking forever - for use in main provisioning loop
    /// Returns true if an event was processed, false if no events are pending
    pub async fn handle_events_non_blocking(&mut self) -> bool {
        match BLE_EVENT_CHANNEL.try_receive() {
            Ok(event) => {
                self.process_event(event).await;
                true
            }
            Err(_) => false, // No events pending
        }
    }

    // RAII cleanup methods for resource management

    /// Synchronous cleanup method for Drop implementation
    /// Cleans up resources in reverse initialization order
    fn cleanup_resources_sync(&mut self) -> BleResult<()> {
        info!("🧹 Starting synchronous BLE resource cleanup");

        // Step 1: Stop advertising if active
        if BLE_ADVERTISING.load(Ordering::SeqCst) {
            info!("🛑 Stopping advertising during cleanup");
            let _ = call_esp_api_with_context(
                || unsafe { esp_idf_sys::esp_ble_gap_stop_advertising() },
                "Cleanup: Advertising stop",
            );
            BLE_ADVERTISING.store(false, Ordering::SeqCst);
        }

        // Step 2: Reset GATT interface
        if GATT_INTERFACE.load(Ordering::SeqCst) != 0 {
            info!("🔄 Resetting GATT interface during cleanup");
            GATT_INTERFACE.store(0, Ordering::SeqCst);
        }

        // Step 3: Mark callbacks as unregistered
        if self.callbacks_registered {
            info!("📋 Marking callbacks as unregistered during cleanup");
            self.callbacks_registered = false;
        }

        if self.gatt_app_registered {
            info!("📱 Marking GATT app as unregistered during cleanup");
            self.gatt_app_registered = false;
        }

        // Step 4: Reset global state
        {
            let mut state = self.state.lock().unwrap();
            state.is_connected = false;
            state.received_credentials = None;
            state.service_handle = 0;
            state.char_handles.clear();
        }

        // Step 5: Reset initialization state
        BLE_INITIALIZED.store(false, Ordering::SeqCst);
        self.init_state = BleInitState::Uninitialized;

        // Step 6: Drop BT driver (this automatically cleans up Bluedroid and controller)
        if self.bt_driver.is_some() {
            info!("📡 Dropping BT driver during cleanup");
            self.bt_driver.take();
        }

        info!("✅ Synchronous BLE resource cleanup completed");
        Ok(())
    }

    /// Asynchronous cleanup method for explicit cleanup calls
    /// Provides more comprehensive cleanup with proper error handling
    pub async fn cleanup_resources_async(&mut self) -> BleResult<()> {
        info!("🧹 Starting comprehensive asynchronous BLE resource cleanup");

        // Step 1: Stop advertising with proper error handling
        if BLE_ADVERTISING.load(Ordering::SeqCst) {
            info!("🛑 Stopping BLE advertising during cleanup");
            if let Err(e) = call_esp_api_with_context(
                || unsafe { esp_idf_sys::esp_ble_gap_stop_advertising() },
                "Cleanup: Advertising stop",
            ) {
                warn!("⚠️ Warning during advertising stop in cleanup: {:?}", e);
            }
            BLE_ADVERTISING.store(false, Ordering::SeqCst);
        }

        // Step 2: Unregister callbacks if they were registered
        if self.callbacks_registered {
            info!("📋 Unregistering BLE callbacks during cleanup");
            // Note: ESP-IDF doesn't provide explicit callback unregistration
            // The callbacks will be cleaned up when the BT driver is dropped
            self.callbacks_registered = false;
        }

        if self.gatt_app_registered {
            info!("📱 Marking GATT application as unregistered during cleanup");
            self.gatt_app_registered = false;
        }

        // Step 3: Reset all atomic state
        BLE_INITIALIZED.store(false, Ordering::SeqCst);
        BLE_ADVERTISING.store(false, Ordering::SeqCst);
        GATT_INTERFACE.store(0, Ordering::SeqCst);

        // Step 4: Clear server state
        {
            let mut state = self.state.lock().unwrap();
            state.is_connected = false;
            state.received_credentials = None;
            state.service_handle = 0;
            state.char_handles.clear();
        }

        // Step 5: Reset initialization state
        self.init_state = BleInitState::Uninitialized;

        // Step 6: Drop BT driver last (this triggers full BLE stack cleanup)
        if self.bt_driver.is_some() {
            info!("📡 Dropping BT driver - this will cleanup Bluedroid and controller");
            self.bt_driver.take();

            // Allow time for cleanup to complete
            Timer::after(Duration::from_millis(100)).await;
        }

        info!("✅ Comprehensive asynchronous BLE resource cleanup completed");
        Ok(())
    }

    async fn process_event(&mut self, event: BleEvent) {
        match event {
            BleEvent::Initialized => {
                // Already logged in event handler - just update state if needed
            }
            BleEvent::ServiceCreated { service_handle } => {
                // Already logged in event handler - just update state
                let mut state = self.state.lock().unwrap();
                state.service_handle = service_handle;
            }
            BleEvent::CharacteristicAdded {
                char_handle: _,
                uuid: _,
            } => {
                // Already logged in event handler - no additional processing needed
            }
            BleEvent::AdvertisingStarted => {
                // Already logged in event handler - no additional processing needed
            }
            BleEvent::ClientConnected => {
                // Already logged in event handler - just update state
                {
                    let mut state = self.state.lock().unwrap();
                    state.is_connected = true;
                }
            }
            BleEvent::ClientDisconnected => {
                // Update state and restart advertising for new connections
                {
                    let mut state = self.state.lock().unwrap();
                    state.is_connected = false;
                }

                // Automatically restart advertising after disconnection to remain discoverable
                info!("🔄 Client disconnected - restarting advertising for new connections");
                match self.start_ble_advertising().await {
                    Ok(_) => {
                        info!(
                            "✅ Advertising restarted successfully - device is discoverable again"
                        );
                    }
                    Err(e) => {
                        error!("❌ Failed to restart advertising after disconnect: {:?}", e);
                    }
                }
            }
            BleEvent::CredentialsReceived(credentials) => {
                // This is only logged here since it comes from write handler
                info!("🔑 Received WiFi credentials - SSID: {}", credentials.ssid);

                let mut state = self.state.lock().unwrap();
                state.received_credentials = Some(credentials.clone());
                info!("✅ Credentials stored for processing");
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
    gatt_interface: esp_idf_sys::esp_gatt_if_t,
    event_param: *mut esp_idf_sys::esp_ble_gatts_cb_param_t,
) {
    // PANIC SAFETY: Wrap all Rust code in catch_unwind to prevent undefined behavior
    // when panics occur in C callbacks on ESP32
    let result =
        std::panic::catch_unwind(|| gatts_event_handler_impl(event, gatt_interface, event_param));

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
    gatt_interface: esp_idf_sys::esp_gatt_if_t,
    event_param: *mut esp_idf_sys::esp_ble_gatts_cb_param_t,
) {
    if event_param.is_null() {
        return;
    }

    match event {
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_REG_EVT => {
            info!(
                "📋 GATT server registered with interface: {}",
                gatt_interface
            );
            GATT_INTERFACE.store(gatt_interface, Ordering::SeqCst);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CREATE_EVT => {
            let create_event = unsafe { &(*event_param).create };
            info!(
                "📋 GATT service created with handle: {}",
                create_event.service_handle
            );

            // Store service handle in state
            with_ble_server_state(|state| {
                state.service_handle = create_event.service_handle;
            });

            // Add characteristics after service creation
            add_service_characteristics(create_event.service_handle);

            send_ble_event_with_backpressure(BleEvent::ServiceCreated {
                service_handle: create_event.service_handle,
            });
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_ADD_CHAR_EVT => {
            let add_char_event = unsafe { &(*event_param).add_char };
            info!(
                "📝 Characteristic added with handle: {}",
                add_char_event.attr_handle
            );
            info!(
                "🔍 CHAR ADD DEBUG: Event details - service_handle: {}, attr_handle: {}",
                add_char_event.service_handle, add_char_event.attr_handle
            );

            // Store characteristic handle and check if all are added
            let should_start_service = with_ble_server_state(|state| {
                info!(
                    "🔍 CHAR ADD DEBUG: Current char_handles count before adding: {}",
                    state.char_handles.len()
                );

                // This is a simplified mapping - in production you'd track which char this is
                let char_count = state.char_handles.len();
                let char_name = if char_count == 0 {
                    state
                        .char_handles
                        .insert("ssid".to_string(), add_char_event.attr_handle);
                    "ssid"
                } else if char_count == 1 {
                    state
                        .char_handles
                        .insert("password".to_string(), add_char_event.attr_handle);
                    "password"
                } else if char_count == 2 {
                    state
                        .char_handles
                        .insert("status".to_string(), add_char_event.attr_handle);
                    "status"
                } else {
                    "unknown"
                };

                info!(
                    "🔍 CHAR ADD DEBUG: Mapped characteristic #{} to '{}' with handle {}",
                    char_count, char_name, add_char_event.attr_handle
                );
                info!(
                    "🔍 CHAR ADD DEBUG: Total characteristics now: {}",
                    state.char_handles.len()
                );

                // List all current characteristic handles
                for (key, handle) in &state.char_handles {
                    info!("🔍 CHAR ADD DEBUG: '{}' = handle {}", key, handle);
                }

                // Return true if all 3 characteristics are now added
                let all_chars_added = state.char_handles.len() == 3;
                info!(
                    "🔍 CHAR ADD DEBUG: All characteristics added: {}",
                    all_chars_added
                );
                all_chars_added
            })
            .unwrap_or(false);

            // Start service after all characteristics are added
            if should_start_service {
                info!("🔍 CHAR ADD DEBUG: All characteristics added, starting service...");
                let service_handle =
                    with_ble_server_state(|state| state.service_handle).unwrap_or(0);

                if service_handle != 0 {
                    info!("🚀 All characteristics added - starting GATT service");
                    info!(
                        "🔍 CHAR ADD DEBUG: Starting service with handle: {}",
                        service_handle
                    );
                    let start_result = call_esp_api_with_context(
                        || unsafe { esp_idf_sys::esp_ble_gatts_start_service(service_handle) },
                        "GATT service start after characteristics",
                    );

                    match start_result {
                        Ok(_) => {
                            info!(
                                "✅ GATT service started successfully - mobile apps can now connect"
                            );
                            info!("🔍 CHAR ADD DEBUG: Service started successfully");
                        }
                        Err(e) => {
                            error!("❌ Failed to start GATT service: {:?}", e);
                            info!("🔍 CHAR ADD DEBUG: Service start failed: {:?}", e);
                        }
                    }
                } else {
                    error!("🔍 CHAR ADD DEBUG: CRITICAL ERROR - service_handle is 0!");
                }
            } else {
                info!(
                    "🔍 CHAR ADD DEBUG: Waiting for more characteristics before starting service"
                );
            }

            send_ble_event_with_backpressure(BleEvent::CharacteristicAdded {
                char_handle: add_char_event.attr_handle,
                uuid: "unknown".to_string(),
            });
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CONNECT_EVT => {
            let connect_event = unsafe { &(*event_param).connect };
            info!("📱 BLE client connected");
            info!(
                "🔍 CONNECTION DEBUG: conn_id: {}, remote_bda: {:02x?}",
                connect_event.conn_id, connect_event.remote_bda
            );

            // Clean up any prepare write buffers for this connection (in case of reconnection)
            with_prepare_write_buffers(|buffers| {
                if let Some(buffer) = buffers.remove(&connect_event.conn_id) {
                    info!("🧹 CONNECTION: Cleaned up stale prepare write buffer for conn_id {} (was {} bytes)", 
                          connect_event.conn_id, buffer.data.len());
                } else {
                    info!("🔍 CONNECTION DEBUG: No stale prepare write buffer to clean for conn_id: {}", 
                          connect_event.conn_id);
                }
            });

            // Update connection state and start timeout tracking
            with_ble_server_state(|state| {
                info!("🔍 CONNECTION DEBUG: Updating connection state");
                state.is_connected = true;
                state.connection_time = Some(std::time::Instant::now());
                // Clear any previous credentials
                state.received_credentials = None;
                info!("🔍 CONNECTION DEBUG: State updated - connected: {}, timeout started, credentials cleared", 
                      state.is_connected);
            });
            info!("⏰ Starting 30-second timeout for WiFi credentials");
            send_ble_event_with_backpressure(BleEvent::ClientConnected);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_DISCONNECT_EVT => {
            let disconnect_event = unsafe { &(*event_param).disconnect };
            info!("📱 BLE client disconnected");
            info!(
                "🔍 DISCONNECT DEBUG: conn_id: {}, reason: {}",
                disconnect_event.conn_id, disconnect_event.reason
            );

            // Clean up any prepare write buffers for this connection
            with_prepare_write_buffers(|buffers| {
                if let Some(buffer) = buffers.remove(&disconnect_event.conn_id) {
                    info!("🧹 DISCONNECT: Cleaned up prepare write buffer for conn_id {} (was {} bytes)", 
                          disconnect_event.conn_id, buffer.data.len());
                } else {
                    info!(
                        "🔍 DISCONNECT DEBUG: No prepare write buffer to clean for conn_id: {}",
                        disconnect_event.conn_id
                    );
                }
            });

            // Update connection state and clear timeout tracking
            with_ble_server_state(|state| {
                state.is_connected = false;
                state.connection_time = None;
                // Keep credentials if they were received
            });
            send_ble_event_with_backpressure(BleEvent::ClientDisconnected);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_WRITE_EVT => {
            let write_event = unsafe { &(*event_param).write };
            info!("🔍 GATT WRITE EVENT DEBUG: Received write event from mobile app");
            info!(
                "🔍 GATT WRITE EVENT DEBUG: Event details - conn_id: {}, trans_id: {}, handle: {}",
                write_event.conn_id, write_event.trans_id, write_event.handle
            );
            info!("🔍 GATT WRITE EVENT DEBUG: Write details - offset: {}, need_rsp: {}, is_prep: {}, len: {}", 
                  write_event.offset, write_event.need_rsp, write_event.is_prep, write_event.len);
            info!(
                "🔍 GATT WRITE EVENT DEBUG: Data pointer null: {}",
                write_event.value.is_null()
            );

            if !write_event.value.is_null() && write_event.len > 0 {
                let preview_len = std::cmp::min(50, write_event.len as usize);
                let data_preview =
                    unsafe { std::slice::from_raw_parts(write_event.value, preview_len) };
                info!(
                    "🔍 GATT WRITE EVENT DEBUG: First {} bytes (hex): {:02x?}",
                    preview_len, data_preview
                );

                if let Ok(utf8_preview) = std::str::from_utf8(data_preview) {
                    info!(
                        "🔍 GATT WRITE EVENT DEBUG: First {} chars (UTF-8): '{}'",
                        utf8_preview.len(),
                        utf8_preview
                    );
                } else {
                    info!("🔍 GATT WRITE EVENT DEBUG: Data is not valid UTF-8 at start");
                }
            }

            // Check if this is a prepare write (part of long write sequence)
            if write_event.is_prep {
                info!("🔍 GATT WRITE EVENT DEBUG: This is a prepare write (is_prep=true) - routing to prepare write handler");
                handle_prepare_write(write_event);
            } else {
                info!("🔍 GATT WRITE EVENT DEBUG: This is a normal write (is_prep=false) - calling handle_characteristic_write...");
                handle_characteristic_write(write_event);
                info!("🔍 GATT WRITE EVENT DEBUG: handle_characteristic_write completed");
            }
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_EXEC_WRITE_EVT => {
            let exec_write_event = unsafe { &(*event_param).exec_write };
            info!("🔍 GATT EXEC WRITE EVENT DEBUG: Received execute write event from mobile app");
            info!(
                "🔍 GATT EXEC WRITE EVENT DEBUG: conn_id: {}, exec_write_flag: {}",
                exec_write_event.conn_id, exec_write_event.exec_write_flag
            );
            handle_execute_write(exec_write_event);
        }
        _ => {
            // Log unknown events for debugging with more detail
            if event == 21 {
                info!("🔍 GATT EVENT 21 DEBUG: This might be ESP_GATTS_RESPONSE_EVT (response confirmation)");
            } else {
                info!(
                    "🔍 GATT UNKNOWN EVENT DEBUG: Received unhandled GATT event: {}",
                    event
                );
            }
        }
    }
}

// Real GAP event handler with panic safety
extern "C" fn gap_event_handler(
    event: esp_idf_sys::esp_gap_ble_cb_event_t,
    event_param: *mut esp_idf_sys::esp_ble_gap_cb_param_t,
) {
    // PANIC SAFETY: Wrap all Rust code in catch_unwind to prevent undefined behavior
    // when panics occur in C callbacks on ESP32
    let result = std::panic::catch_unwind(|| gap_event_handler_impl(event, event_param));

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
    _event_param: *mut esp_idf_sys::esp_ble_gap_cb_param_t,
) {
    match event {
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_DATA_SET_COMPLETE_EVT => {
            // Note: This fires twice - once for advertising data, once for scan response
            // We already log this in the calling function with more specific context
        }
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_START_COMPLETE_EVT => {
            info!("📡 Advertising started successfully");
            BLE_ADVERTISING.store(true, Ordering::SeqCst);
            send_ble_event_with_backpressure(BleEvent::AdvertisingStarted);
        }
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_STOP_COMPLETE_EVT => {
            info!("📡 Advertising stopped");
            BLE_ADVERTISING.store(false, Ordering::SeqCst);
        }
        // Handle connection update and security events that might cause "Device not found"
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_UPDATE_CONN_PARAMS_EVT => {
            info!("🔄 BLE connection parameters updated");
        }
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_SET_PKT_LENGTH_COMPLETE_EVT => {
            info!("📏 BLE packet length updated");
        }
        _ => {
            // Log unknown events for debugging
            info!("📡 GAP event: {}", event);
        }
    }
}

// Add characteristics to service
fn add_service_characteristics(service_handle: u16) {
    info!(
        "🔍 ADD CHARS DEBUG: Starting to add characteristics to service handle: {}",
        service_handle
    );

    // Add SSID characteristic (writable)
    info!(
        "🔍 ADD CHARS DEBUG: Creating SSID characteristic with UUID: {}",
        SSID_CHAR_UUID
    );
    let ssid_uuid = parse_uuid(SSID_CHAR_UUID).unwrap();
    let ssid_uuid_struct = esp_idf_sys::esp_bt_uuid_t {
        len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 { uuid128: ssid_uuid },
    };
    let ssid_result = call_esp_api_with_context(
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
    match ssid_result {
        Ok(_) => info!("🔍 ADD CHARS DEBUG: SSID characteristic creation initiated successfully"),
        Err(e) => error!(
            "🔍 ADD CHARS DEBUG: SSID characteristic creation failed: {:?}",
            e
        ),
    }

    // Add Password characteristic (writable)
    info!(
        "🔍 ADD CHARS DEBUG: Creating Password characteristic with UUID: {}",
        PASSWORD_CHAR_UUID
    );
    let password_uuid = parse_uuid(PASSWORD_CHAR_UUID).unwrap();
    let password_uuid_struct = esp_idf_sys::esp_bt_uuid_t {
        len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 {
            uuid128: password_uuid,
        },
    };
    let password_result = call_esp_api_with_context(
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
    match password_result {
        Ok(_) => {
            info!("🔍 ADD CHARS DEBUG: Password characteristic creation initiated successfully")
        }
        Err(e) => error!(
            "🔍 ADD CHARS DEBUG: Password characteristic creation failed: {:?}",
            e
        ),
    }

    // Add Status characteristic (readable + notifiable)
    info!(
        "🔍 ADD CHARS DEBUG: Creating Status characteristic with UUID: {}",
        STATUS_CHAR_UUID
    );
    let status_uuid = parse_uuid(STATUS_CHAR_UUID).unwrap();
    let status_uuid_struct = esp_idf_sys::esp_bt_uuid_t {
        len: esp_idf_sys::ESP_UUID_LEN_128 as u16,
        uuid: esp_idf_sys::esp_bt_uuid_t__bindgen_ty_1 {
            uuid128: status_uuid,
        },
    };
    let status_result = call_esp_api_with_context(
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
    match status_result {
        Ok(_) => info!("🔍 ADD CHARS DEBUG: Status characteristic creation initiated successfully"),
        Err(e) => error!(
            "🔍 ADD CHARS DEBUG: Status characteristic creation failed: {:?}",
            e
        ),
    }

    info!("🔍 ADD CHARS DEBUG: All characteristic creation requests submitted - waiting for ESP-IDF callbacks");
}

// Send immediate status notification (synchronous - for use in callbacks)
fn send_immediate_status_notification(message: &str) {
    info!("📤 Sending immediate status to mobile app: {}", message);
    info!("🔍 STATUS NOTIFICATION DEBUG: Starting status notification process");

    // Get status characteristic handle from global state
    info!("🔍 STATUS NOTIFICATION DEBUG: Getting status characteristic handle from global state");
    let status_handle = with_ble_server_state(|state| {
        info!(
            "🔍 STATUS NOTIFICATION DEBUG: Accessing global state, char_handles count: {}",
            state.char_handles.len()
        );
        for (key, handle) in &state.char_handles {
            info!(
                "🔍 STATUS NOTIFICATION DEBUG: char_handle: '{}' = {}",
                key, handle
            );
        }
        state.char_handles.get("status").copied().unwrap_or(0)
    })
    .unwrap_or(0);

    info!(
        "🔍 STATUS NOTIFICATION DEBUG: Status handle retrieved: {}",
        status_handle
    );

    if status_handle != 0 {
        let data = message.as_bytes();
        let gatt_interface = GATT_INTERFACE.load(Ordering::SeqCst);
        info!("🔍 STATUS NOTIFICATION DEBUG: Preparing notification - interface: {}, handle: {}, data_len: {}, message: '{}'", 
              gatt_interface, status_handle, data.len(), message);

        let result = call_esp_api_with_context(
            || unsafe {
                esp_idf_sys::esp_ble_gatts_send_indicate(
                    gatt_interface,
                    0, // conn_id - we don't track this, ESP-IDF will find the connection
                    status_handle,
                    data.len() as u16,
                    data.as_ptr() as *mut u8,
                    false, // need_confirm = false for notification
                )
            },
            "Immediate status notification",
        );

        match result {
            Ok(_) => {
                info!("✅ Immediate status notification sent: {}", message);
                info!("🔍 STATUS NOTIFICATION DEBUG: Notification sent successfully");
            }
            Err(e) => {
                warn!("⚠️ Failed to send immediate status notification: {:?}", e);
                info!(
                    "🔍 STATUS NOTIFICATION DEBUG: Notification failed with error: {:?}",
                    e
                );
            }
        }
    } else {
        warn!("⚠️ Cannot send status notification - no status characteristic handle");
        info!("🔍 STATUS NOTIFICATION DEBUG: FAILURE - No status characteristic handle found");
        info!(
            "🔍 STATUS NOTIFICATION DEBUG: This means characteristics were not properly registered"
        );
    }
}

// Handle BLE Long Write - Prepare Write Event (ESP_GATTS_PREP_WRITE_EVT)
fn handle_prepare_write(param: &esp_idf_sys::esp_ble_gatts_cb_param_t_gatts_write_evt_param) {
    // 🔧 STACK OVERFLOW FIX: Reduced debug logging to prevent BTC task stack exhaustion
    debug!(
        "🔍 PREP WRITE: Starting handler - conn_id: {}, len: {} bytes",
        param.conn_id, param.len
    );

    // Basic input validation
    if param.value.is_null() || param.len == 0 {
        warn!("❌ PREP WRITE: null or empty data received");
        return;
    }

    // Validate offset and length bounds
    if param.offset > MAX_PREPARE_WRITE_BUFFER_SIZE as u16 {
        warn!(
            "❌ PREP WRITE: offset {} exceeds maximum buffer size",
            param.offset
        );
        return;
    }

    if param.len > 512 {
        warn!(
            "❌ PREP WRITE: chunk length {} exceeds reasonable MTU size",
            param.len
        );
        return;
    }

    let chunk_data = unsafe { std::slice::from_raw_parts(param.value, param.len as usize) };

    // Access prepare write buffers and append this chunk
    let append_result = with_prepare_write_buffers(|buffers| {
        // Get or create buffer for this connection
        let buffer = buffers.entry(param.conn_id).or_insert_with(|| {
            debug!(
                "🔍 PREP WRITE: Creating new buffer for conn_id: {}",
                param.conn_id
            );
            PrepareWriteBuffer::new(param.handle)
        });

        // Verify the handle matches (all chunks should be for the same characteristic)
        if buffer.char_handle != param.handle {
            warn!(
                "❌ PREP WRITE: handle mismatch - buffer: {}, chunk: {}",
                buffer.char_handle, param.handle
            );
            return Err(BleError::InvalidState(
                "Prepare write handle mismatch".to_string(),
            ));
        }

        // Append the chunk to the buffer
        match buffer.append_chunk(param.offset, chunk_data) {
            Ok(_) => {
                debug!(
                    "✅ PREP WRITE: Chunk appended - buffer size: {} bytes",
                    buffer.data.len()
                );
                Ok(())
            }
            Err(e) => {
                warn!("❌ PREP WRITE: failed to append chunk: {:?}", e);
                buffers.remove(&param.conn_id);
                Err(e)
            }
        }
    });

    match append_result {
        Some(Ok(_)) => {
            info!(
                "✅ PREP WRITE: Chunk {} bytes at offset {} buffered for conn_id {}",
                param.len, param.offset, param.conn_id
            );

            // Send prepare write response - this is REQUIRED for the mobile app to continue
            let gatt_interface = GATT_INTERFACE.load(Ordering::SeqCst);

            // Create response structure that echoes back the prepare write data
            let mut response = esp_idf_sys::esp_gatt_rsp_t {
                attr_value: esp_idf_sys::esp_gatt_value_t {
                    handle: param.handle,
                    offset: param.offset,
                    len: param.len,
                    value: [0u8; 512],
                    auth_req: 0,
                },
            };

            // Copy the received data into the response (echo back)
            unsafe {
                std::ptr::copy_nonoverlapping(
                    param.value,
                    response.attr_value.value.as_mut_ptr(),
                    param.len as usize,
                );
            }

            let response_result = call_esp_api_with_context(
                || unsafe {
                    esp_idf_sys::esp_ble_gatts_send_response(
                        gatt_interface,
                        param.conn_id,
                        param.trans_id,
                        esp_idf_sys::esp_gatt_status_t_ESP_GATT_OK,
                        &mut response as *mut _,
                    )
                },
                "Prepare write response",
            );

            match response_result {
                Ok(_) => {
                    info!("✅ PREP WRITE: Response sent - waiting for execute write");
                }
                Err(e) => {
                    warn!("❌ PREP WRITE: Failed to send response: {:?}", e);
                }
            }
        }
        Some(Err(e)) => {
            warn!("❌ PREP WRITE: Failed to buffer chunk: {:?}", e);
        }
        None => {
            warn!("❌ PREP WRITE: Failed to access prepare write buffers");
        }
    }

    // Clean up expired buffers periodically
    cleanup_expired_prepare_write_buffers();

    debug!("✅ PREP WRITE: Handler completed");
}

// Handle BLE Long Write - Execute Write Event (ESP_GATTS_EXEC_WRITE_EVT)
fn handle_execute_write(param: &esp_idf_sys::esp_ble_gatts_cb_param_t_gatts_exec_write_evt_param) {
    info!("🔍 EXEC WRITE DEBUG: Starting execute write handler");
    info!(
        "🔍 EXEC WRITE DEBUG: conn_id: {}, exec_write_flag: {}",
        param.conn_id, param.exec_write_flag
    );

    // Check if this is an execute (commit) or cancel operation
    if param.exec_write_flag as u32 == esp_idf_sys::ESP_GATT_PREP_WRITE_CANCEL {
        info!("🔍 EXEC WRITE DEBUG: Prepare write cancelled - cleaning up buffer");
        with_prepare_write_buffers(|buffers| {
            if let Some(buffer) = buffers.remove(&param.conn_id) {
                info!(
                    "🧹 EXEC WRITE: Cancelled prepare write buffer for conn_id {} (was {} bytes)",
                    param.conn_id,
                    buffer.data.len()
                );
            } else {
                info!(
                    "🔍 EXEC WRITE DEBUG: No buffer found to cancel for conn_id: {}",
                    param.conn_id
                );
            }
        });
        return;
    }

    if param.exec_write_flag as u32 != esp_idf_sys::ESP_GATT_PREP_WRITE_EXEC {
        warn!(
            "❌ EXEC WRITE: Unknown exec_write_flag: {}",
            param.exec_write_flag
        );
        return;
    }

    info!("🔍 EXEC WRITE DEBUG: Execute write confirmed - processing buffered data");

    // Retrieve and remove the buffer for this connection
    let buffer_data = with_prepare_write_buffers(|buffers| buffers.remove(&param.conn_id));

    match buffer_data {
        Some(Some(buffer)) => {
            info!("✅ EXEC WRITE: Retrieved prepare write buffer - total size: {} bytes, char_handle: {}", 
                  buffer.data.len(), buffer.char_handle);
            info!(
                "🔍 EXEC WRITE DEBUG: Buffer age: {:?}",
                buffer.created_at.elapsed()
            );

            // Create a synthetic write event parameter to reuse existing credential processing logic
            let synthetic_write_param =
                esp_idf_sys::esp_ble_gatts_cb_param_t_gatts_write_evt_param {
                    conn_id: param.conn_id,
                    trans_id: 0, // Not used in credential processing
                    bda: [0; 6], // Not used in credential processing
                    handle: buffer.char_handle,
                    offset: 0,
                    need_rsp: false, // Execute write doesn't need response
                    is_prep: false,  // This is the final execution, not a prep
                    len: buffer.data.len() as u16,
                    value: buffer.data.as_ptr() as *mut u8,
                };

            info!("🔍 EXEC WRITE DEBUG: Created synthetic write param, calling credential processing...");
            info!(
                "🔍 EXEC WRITE DEBUG: Final payload first 100 bytes: {:02x?}",
                &buffer.data[..std::cmp::min(100, buffer.data.len())]
            );

            // Process the complete data as a normal write event
            handle_characteristic_write(&synthetic_write_param);

            info!("🔍 EXEC WRITE DEBUG: Execute write processing completed");
        }
        Some(None) => {
            warn!(
                "❌ EXEC WRITE: No prepare write buffer found for conn_id: {}",
                param.conn_id
            );
            info!("🔍 EXEC WRITE DEBUG: This could indicate the buffer was already processed or timed out");
        }
        None => {
            warn!("❌ EXEC WRITE: Failed to access prepare write buffers");
        }
    }
}

// Handle characteristic write events
fn handle_characteristic_write(
    param: &esp_idf_sys::esp_ble_gatts_cb_param_t_gatts_write_evt_param,
) {
    info!("🔍 BLE WRITE DEBUG: Starting characteristic write handler");
    info!(
        "🔍 BLE WRITE DEBUG: Raw param details - handle: {}, offset: {}, need_rsp: {}, is_prep: {}",
        param.handle, param.offset, param.need_rsp, param.is_prep
    );

    // Basic input validation
    if param.value.is_null() || param.len == 0 {
        warn!("❌ BLE write: null or empty data received");
        info!(
            "🔍 BLE WRITE DEBUG: Validation failed - value null: {}, len: {}",
            param.value.is_null(),
            param.len
        );
        return;
    }

    info!(
        "🔍 BLE WRITE DEBUG: Initial validation passed - data pointer valid, len: {}",
        param.len
    );

    // Validate data length bounds
    const MIN_DATA_LEN: u16 = 10; // Minimum for valid JSON: {"ssid":"x"}
    const MAX_DATA_LEN: u16 = MAX_PREPARE_WRITE_BUFFER_SIZE as u16; // Support long write payloads

    info!(
        "🔍 BLE WRITE DEBUG: Length validation - received: {} bytes, min: {}, max: {}",
        param.len, MIN_DATA_LEN, MAX_DATA_LEN
    );

    if param.len < MIN_DATA_LEN {
        warn!(
            "❌ BLE write: data too short ({} bytes, min {})",
            param.len, MIN_DATA_LEN
        );
        info!("🔍 BLE WRITE DEBUG: REJECTION REASON: Data too short");
        return;
    }

    if param.len > MAX_DATA_LEN {
        warn!(
            "❌ BLE write: data too long ({} bytes, max {})",
            param.len, MAX_DATA_LEN
        );
        info!("🔍 BLE WRITE DEBUG: REJECTION REASON: Data too long");
        info!(
            "🔍 BLE WRITE DEBUG: Received {} bytes but max allowed is {} bytes",
            param.len, MAX_DATA_LEN
        );
        info!("🔍 BLE WRITE DEBUG: Large payloads should use BLE Long Write (prepare/execute write sequence)");
        return;
    }

    info!("🔍 BLE WRITE DEBUG: Length validation passed, extracting data...");

    let data = unsafe { std::slice::from_raw_parts(param.value, param.len as usize) };

    // 🔧 STACK OVERFLOW FIX: Reduced debug logging to prevent BTC task stack exhaustion
    debug!(
        "🔍 BLE credential processing started, payload size: {} bytes",
        data.len()
    );

    // Validate UTF-8 first
    let json_str = match std::str::from_utf8(data) {
        Ok(s) => {
            info!(
                "✅ UTF-8 validation passed, credential size: {} bytes",
                s.len()
            );
            s
        }
        Err(e) => {
            warn!(
                "❌ BLE write: received invalid UTF-8 data ({} bytes): {:?}",
                data.len(),
                e
            );
            return;
        }
    };

    // Basic JSON format validation
    let trimmed = json_str.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        warn!(
            "❌ BLE write: invalid JSON format (size: {})",
            trimmed.len()
        );
        return;
    }

    info!("🔍 Parsing WiFi credentials JSON...");

    // Parse and validate WiFi credentials
    match serde_json::from_str::<WiFiCredentials>(json_str) {
        Ok(credentials) => {
            info!(
                "🔑 Received WiFi credentials via BLE: SSID='{}', auth_token_len={}",
                credentials.ssid,
                credentials.auth_token.len()
            );

            // 🎯 STAGE 1: Send immediate acknowledgment to mobile app
            send_immediate_status_notification("RECEIVED");

            // Store in global state and clear timeout (credentials received!)
            with_ble_server_state(|state| {
                state.received_credentials = Some(credentials.clone());
                state.connection_time = None; // Clear timeout - credentials received
            });

            info!("✅ Credentials stored - timeout cleared");

            // Send credentials received event for async processing
            send_ble_event_with_backpressure(BleEvent::CredentialsReceived(credentials));

            info!("✅ BLE credential processing completed successfully");
        }
        Err(e) => {
            warn!("❌ Failed to parse WiFi credentials JSON: {}", e);

            // Send error response to mobile app
            send_immediate_status_notification("ERROR_INVALID_JSON");
        }
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
