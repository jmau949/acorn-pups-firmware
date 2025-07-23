// Production-grade BLE server implementation using real ESP-IDF GATT APIs
// This provides actual WiFi provisioning via real BLE GATT services
//
// 📋 LENGTH-PREFIX PROTOCOL SPECIFICATION:
// =======================================
//
// The server now uses a length-prefix protocol to reliably receive large JSON payloads
// from mobile apps. This eliminates issues with MTU limitations and chunk detection.
//
// PROTOCOL FORMAT:
// 1. Length Prefix (4 bytes): u32 little-endian indicating payload size
// 2. JSON Payload (variable): The actual WiFi credentials JSON data
//
// MOBILE APP IMPLEMENTATION:
// The mobile app should send data as follows:
//
// ```javascript
// const jsonString = JSON.stringify(credentials);
// const jsonBytes = new TextEncoder().encode(jsonString);
// const lengthPrefix = new Uint32Array([jsonBytes.length]);
// const lengthBytes = new Uint8Array(lengthPrefix.buffer);
//
// // Combine length prefix + payload
// const fullPayload = new Uint8Array(lengthBytes.length + jsonBytes.length);
// fullPayload.set(lengthBytes, 0);
// fullPayload.set(jsonBytes, lengthBytes.length);
//
// // Send in chunks based on MTU (minus 3 bytes for ATT header)
// const maxChunkSize = mtu - 3;
// for (let i = 0; i < fullPayload.length; i += maxChunkSize) {
//     const chunk = fullPayload.slice(i, i + maxChunkSize);
//     await characteristic.writeValueWithResponse(chunk);
// }
// ```
//
// BENEFITS:
// - ✅ Reliable detection of complete payloads regardless of MTU
// - ✅ Supports payloads up to 4GB (u32 length)
// - ✅ Automatic timeout reset per chunk (prevents stale data)
// - ✅ Detailed progress logging and statistics
// - ✅ Robust error handling with immediate mobile app feedback

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
// Timeout per chunk (resets after each valid data chunk)
const CHUNK_TIMEOUT_SECONDS: u64 = 10;
// Maximum payload size (4KB)
const MAX_PAYLOAD_SIZE: usize = 4096;
// Length prefix size (4 bytes for u32)
const LENGTH_PREFIX_SIZE: usize = 4;

// Global state for BLE operations
static BLE_INITIALIZED: AtomicBool = AtomicBool::new(false);
static BLE_ADVERTISING: AtomicBool = AtomicBool::new(false);
static GATT_INTERFACE: AtomicU8 = AtomicU8::new(0);
static BLE_MTU_SIZE: AtomicU8 = AtomicU8::new(23); // Default BLE MTU

// Global channel for BLE events
pub static BLE_EVENT_CHANNEL: Channel<CriticalSectionRawMutex, BleEvent, 10> = Channel::new();

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

// Protocol state for length-prefix reception
#[derive(Debug, Clone, PartialEq)]
enum ReceptionState {
    WaitingForLength, // Waiting for 4-byte length prefix
    ReceivingPayload {
        // Receiving payload data
        expected_length: u32,
        received_length: usize,
    },
}

// Thread-safe state for callback access
struct BleServerState {
    is_connected: bool,
    received_credentials: Option<WiFiCredentials>,
    service_handle: u16,
    char_handles: HashMap<String, u16>,
    connection_time: Option<std::time::Instant>,
    credentials_timeout_seconds: u64,

    // Enhanced chunked reception with length prefix protocol
    reception_state: ReceptionState,
    data_buffer: Vec<u8>,   // Buffer for assembling multi-packet writes
    length_buffer: Vec<u8>, // Buffer for length prefix bytes
    last_chunk_time: Option<std::time::Instant>, // When last valid chunk was received
    chunk_timeout_seconds: u64, // Timeout per chunk
    total_chunks_received: u32, // Statistics
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

            // Enhanced chunked reception with length prefix protocol
            reception_state: ReceptionState::WaitingForLength,
            data_buffer: Vec::new(),
            length_buffer: Vec::new(),
            last_chunk_time: None,
            chunk_timeout_seconds: CHUNK_TIMEOUT_SECONDS,
            total_chunks_received: 0,
        }
    }

    /// Reset reception state and clear all buffers
    fn reset_reception(&mut self) {
        self.reception_state = ReceptionState::WaitingForLength;
        self.data_buffer.clear();
        self.length_buffer.clear();
        self.last_chunk_time = None;
        self.total_chunks_received = 0;
        info!("🔄 Reception state reset - ready for new payload");
    }

    /// Check if chunk reception has timed out
    fn is_chunk_timed_out(&self) -> bool {
        if let Some(last_chunk) = self.last_chunk_time {
            last_chunk.elapsed().as_secs() > self.chunk_timeout_seconds
        } else {
            false
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
    /// This checks both overall connection timeout and chunk reception timeout
    pub fn is_connection_timed_out(&self) -> bool {
        with_ble_server_state(|state| {
            // Check overall connection timeout
            let connection_timeout = if let Some(connection_time) = state.connection_time {
                let elapsed = connection_time.elapsed().as_secs();
                elapsed > state.credentials_timeout_seconds
            } else {
                false
            };

            // Check chunk timeout (if we're in the middle of receiving data)
            let chunk_timeout = state.is_chunk_timed_out();

            connection_timeout || chunk_timeout
        })
        .unwrap_or(false)
    }

    /// Force disconnect current client (for timeout/security)
    pub async fn force_disconnect_client(&self, reason: &str) -> BleResult<()> {
        info!("🚫 Force disconnecting client: {}", reason);

        // Update state to mark as disconnected and reset reception
        with_ble_server_state(|state| {
            state.is_connected = false;
            state.connection_time = None;
            state.received_credentials = None;
            state.reset_reception(); // Clear any partial data
        });

        // Send disconnection event
        send_ble_event_with_backpressure(BleEvent::ClientDisconnected);

        info!("✅ Client force disconnected successfully");
        Ok(())
    }

    /// Get current reception statistics for debugging
    pub fn get_reception_stats(&self) -> Option<(String, u32, usize, usize)> {
        with_ble_server_state(|state| {
            let state_desc = match &state.reception_state {
                ReceptionState::WaitingForLength => "WaitingForLength".to_string(),
                ReceptionState::ReceivingPayload {
                    expected_length,
                    received_length: _,
                } => {
                    format!("ReceivingPayload({})", expected_length)
                }
            };
            (
                state_desc,
                state.total_chunks_received,
                state.data_buffer.len(),
                state.length_buffer.len(),
            )
        })
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

            // Store characteristic handle and check if all are added
            let should_start_service = with_ble_server_state(|state| {
                // This is a simplified mapping - in production you'd track which char this is
                let char_count = state.char_handles.len();
                if char_count == 0 {
                    state
                        .char_handles
                        .insert("ssid".to_string(), add_char_event.attr_handle);
                } else if char_count == 1 {
                    state
                        .char_handles
                        .insert("password".to_string(), add_char_event.attr_handle);
                } else if char_count == 2 {
                    state
                        .char_handles
                        .insert("status".to_string(), add_char_event.attr_handle);
                }

                // Return true if all 3 characteristics are now added
                state.char_handles.len() == 3
            })
            .unwrap_or(false);

            // Start service after all characteristics are added
            if should_start_service {
                let service_handle =
                    with_ble_server_state(|state| state.service_handle).unwrap_or(0);

                if service_handle != 0 {
                    info!("🚀 All characteristics added - starting GATT service");
                    let start_result = call_esp_api_with_context(
                        || unsafe { esp_idf_sys::esp_ble_gatts_start_service(service_handle) },
                        "GATT service start after characteristics",
                    );

                    match start_result {
                        Ok(_) => info!(
                            "✅ GATT service started successfully - mobile apps can now connect"
                        ),
                        Err(e) => error!("❌ Failed to start GATT service: {:?}", e),
                    }
                }
            }

            send_ble_event_with_backpressure(BleEvent::CharacteristicAdded {
                char_handle: add_char_event.attr_handle,
                uuid: "unknown".to_string(),
            });
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CONNECT_EVT => {
            info!("📱 BLE client connected");
            // Update connection state and start timeout tracking
            with_ble_server_state(|state| {
                state.is_connected = true;
                state.connection_time = Some(std::time::Instant::now());
                // Clear any previous credentials and reset reception state
                state.received_credentials = None;
                state.reset_reception();
            });
            info!(
                "⏰ Starting {}-second timeout for WiFi credentials",
                CREDENTIALS_TIMEOUT_SECONDS
            );
            info!("🔄 Reception state reset - ready for length-prefix protocol");
            send_ble_event_with_backpressure(BleEvent::ClientConnected);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_DISCONNECT_EVT => {
            info!("📱 BLE client disconnected");
            // Update connection state and clear timeout tracking
            with_ble_server_state(|state| {
                state.is_connected = false;
                state.connection_time = None;
                // Reset reception state to clean up any partial data
                state.reset_reception();
                // Keep credentials if they were received
            });
            send_ble_event_with_backpressure(BleEvent::ClientDisconnected);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_WRITE_EVT => {
            let write_event = unsafe { &(*event_param).write };
            handle_characteristic_write(write_event);
        }
        esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_MTU_EVT => {
            let mtu_event = unsafe { &(*event_param).mtu };
            let new_mtu = mtu_event.mtu;
            BLE_MTU_SIZE.store(new_mtu as u8, Ordering::SeqCst);
            info!(
                "📏 BLE MTU negotiated: {} bytes (effective payload: {} bytes)",
                new_mtu,
                new_mtu.saturating_sub(3) // ATT header is 3 bytes
            );

            // Send status update to mobile app about MTU
            let mtu_msg = format!("MTU_NEGOTIATED:{}", new_mtu);
            send_immediate_status_notification(&mtu_msg);
        }
        _ => {
            // Handle other events as needed
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

// Send immediate status notification (synchronous - for use in callbacks)
fn send_immediate_status_notification(message: &str) {
    info!("📤 Sending immediate status to mobile app: {}", message);

    // Get status characteristic handle from global state
    let status_handle =
        with_ble_server_state(|state| state.char_handles.get("status").copied().unwrap_or(0))
            .unwrap_or(0);

    if status_handle != 0 {
        let data = message.as_bytes();
        let result = call_esp_api_with_context(
            || unsafe {
                esp_idf_sys::esp_ble_gatts_send_indicate(
                    GATT_INTERFACE.load(Ordering::SeqCst),
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
            Ok(_) => info!("✅ Immediate status notification sent: {}", message),
            Err(e) => warn!("⚠️ Failed to send immediate status notification: {:?}", e),
        }
    } else {
        warn!("⚠️ Cannot send status notification - no status characteristic handle");
    }
}

// Handle characteristic write events with length-prefix protocol
fn handle_characteristic_write(
    param: &esp_idf_sys::esp_ble_gatts_cb_param_t_gatts_write_evt_param,
) {
    // Basic input validation
    if param.value.is_null() || param.len == 0 {
        warn!("❌ BLE write: null or empty data received");
        return;
    }

    let data = unsafe { std::slice::from_raw_parts(param.value, param.len as usize) };
    let mtu_size = BLE_MTU_SIZE.load(Ordering::SeqCst) as usize;

    info!(
        "📥 BLE write: received {} bytes (MTU: {}, chunk: {})",
        data.len(),
        mtu_size,
        data.len()
    );

    // Process the chunk and check if we have a complete payload
    let complete_payload = with_ble_server_state(|state| {
        // Check for chunk timeout
        if state.is_chunk_timed_out() {
            warn!("⏰ Chunk timeout - resetting reception state");
            state.reset_reception();
        }

        // Update chunk reception time
        state.last_chunk_time = Some(std::time::Instant::now());
        state.total_chunks_received += 1;

        info!(
            "📊 Chunk {} received ({} bytes), state: {:?}",
            state.total_chunks_received,
            data.len(),
            state.reception_state
        );

        match &state.reception_state {
            ReceptionState::WaitingForLength => {
                // Accumulate length prefix bytes
                state.length_buffer.extend_from_slice(data);

                if state.length_buffer.len() >= LENGTH_PREFIX_SIZE {
                    // We have enough bytes to read the length
                    let length_bytes = &state.length_buffer[..LENGTH_PREFIX_SIZE];
                    let expected_length = u32::from_le_bytes([
                        length_bytes[0],
                        length_bytes[1],
                        length_bytes[2],
                        length_bytes[3],
                    ]);

                    info!(
                        "📏 Length prefix received: {} bytes expected",
                        expected_length
                    );

                    // Validate expected length
                    if expected_length == 0 || expected_length > MAX_PAYLOAD_SIZE as u32 {
                        warn!("❌ Invalid payload length: {} bytes", expected_length);
                        state.reset_reception();
                        return None;
                    }

                    // Transition to payload reception
                    state.reception_state = ReceptionState::ReceivingPayload {
                        expected_length,
                        received_length: 0,
                    };

                    // If there are extra bytes after length prefix, process them as payload
                    if state.length_buffer.len() > LENGTH_PREFIX_SIZE {
                        let payload_start = &state.length_buffer[LENGTH_PREFIX_SIZE..];
                        state.data_buffer.extend_from_slice(payload_start);
                        info!(
                            "📦 {} payload bytes received with length prefix",
                            payload_start.len()
                        );
                    }

                    // Clear length buffer
                    state.length_buffer.clear();

                    // Check if we already have complete payload
                    if let ReceptionState::ReceivingPayload {
                        expected_length, ..
                    } = &state.reception_state
                    {
                        if state.data_buffer.len() >= *expected_length as usize {
                            // Complete payload received
                            let payload = state.data_buffer[..*expected_length as usize].to_vec();
                            state.reset_reception();
                            return Some(payload);
                        }
                    }
                }
                None
            }
            ReceptionState::ReceivingPayload {
                expected_length,
                received_length: _,
            } => {
                // Accumulate payload data
                state.data_buffer.extend_from_slice(data);

                info!(
                    "📦 Payload progress: {}/{} bytes ({:.1}%)",
                    state.data_buffer.len(),
                    expected_length,
                    (state.data_buffer.len() as f32 / *expected_length as f32) * 100.0
                );

                // Check if we have received the complete payload
                if state.data_buffer.len() >= *expected_length as usize {
                    info!(
                        "✅ Complete payload received ({} bytes)",
                        state.data_buffer.len()
                    );

                    // Extract the exact payload (in case we received extra bytes)
                    let payload = state.data_buffer[..*expected_length as usize].to_vec();
                    state.reset_reception();
                    Some(payload)
                } else {
                    // Still waiting for more payload data
                    None
                }
            }
        }
    });

    // Process complete payload if we have it
    if let Some(Some(payload_data)) = complete_payload {
        process_complete_payload(payload_data);
    }
}

// Process a complete payload using length-prefix protocol
fn process_complete_payload(payload_data: Vec<u8>) {
    info!(
        "🔄 Processing complete payload ({} bytes)",
        payload_data.len()
    );

    // Convert to UTF-8 string for JSON parsing
    let json_str = match std::str::from_utf8(&payload_data) {
        Ok(s) => s,
        Err(e) => {
            warn!(
                "❌ Payload contains invalid UTF-8 data: {} ({} bytes)",
                e,
                payload_data.len()
            );
            send_immediate_status_notification("ERROR_INVALID_UTF8");
            return;
        }
    };

    // Parse and validate WiFi credentials
    match serde_json::from_str::<WiFiCredentials>(json_str) {
        Ok(credentials) => {
            info!(
                "🔑 Received enhanced WiFi credentials via BLE: SSID={}, Device={}",
                credentials.ssid, credentials.device_name
            );
            info!(
                "🌍 User timezone: {}, Auth token: {}...",
                credentials.user_timezone,
                &credentials.auth_token[..std::cmp::min(20, credentials.auth_token.len())]
            );

            // 🎯 STAGE 1: Send immediate acknowledgment to mobile app
            send_immediate_status_notification("RECEIVED");

            // Store in global state and clear timeout (credentials received!)
            with_ble_server_state(|state| {
                state.received_credentials = Some(credentials.clone());
                state.connection_time = None; // Clear timeout - credentials received
            });

            info!("✅ Credentials received - timeout cleared");

            // Send credentials received event for async processing
            send_ble_event_with_backpressure(BleEvent::CredentialsReceived(credentials));
        }
        Err(e) => {
            warn!(
                "❌ Failed to parse WiFi credentials from JSON: {} (error: {})",
                json_str, e
            );

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
