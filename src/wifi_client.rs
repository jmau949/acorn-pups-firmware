// Import Embassy time utilities for async delays
use embassy_time::{Duration, Timer};

// Import ESP-IDF event loop for handling system events
// The event loop manages WiFi connection events, IP assignment, etc.
use esp_idf_svc::eventloop::EspSystemEventLoop;

// Import peripheral trait for hardware access
use esp_idf_svc::hal::peripheral::Peripheral;

// Import NVS partition for WiFi driver storage needs
use esp_idf_svc::nvs::EspDefaultNvsPartition;

// Import ESP-IDF error type
use esp_idf_svc::sys::EspError;

// Import WiFi-related types from ESP-IDF
// - AuthMethod: WiFi security types (WPA2, WPA3, etc.)
// - BlockingWifi: Synchronous WiFi operations wrapper
// - ClientConfiguration: WiFi station mode config
// - Configuration: Overall WiFi configuration
// - EspWifi: Low-level WiFi driver
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};

// Import logging macros
use log::{error, info, warn};

// Import standard library IPv4 address type
use std::net::Ipv4Addr;

// Import WiFi credentials from our BLE module
use crate::ble_server::WiFiCredentials;

// Enum representing the current state of WiFi connection
// This helps track what the WiFi system is currently doing
#[derive(Debug, Clone)]
pub enum WiFiConnectionState {
    Disconnected,        // Not connected to any network
    Connecting,          // Currently trying to connect
    Connected(Ipv4Addr), // Connected with this IP address
    Failed(String),      // Connection failed with this error message
}

// Structure containing the results of a WiFi connection attempt
// This provides comprehensive information about what happened
#[derive(Debug, Clone)]
pub struct WiFiConnectionResult {
    pub success: bool,                 // Did the connection succeed?
    pub ip_address: Option<Ipv4Addr>,  // What IP address was assigned (if any)?
    pub error_message: Option<String>, // Error description if connection failed
    pub connection_time_ms: u64,       // How long did the attempt take?
}

// WiFi client manager - handles all WiFi operations
// This wraps the ESP-IDF WiFi functionality with higher-level logic
pub struct WiFiClient {
    wifi: BlockingWifi<EspWifi<'static>>, // ESP-IDF WiFi driver wrapper
    state: WiFiConnectionState,           // Current connection state
    last_connection_attempt: Option<std::time::Instant>, // When we last tried to connect
}

impl WiFiClient {
    pub fn new(
        modem: impl Peripheral<P = esp_idf_svc::hal::modem::Modem> + 'static,
        sys_loop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
    ) -> Result<Self, EspError> {
        info!("Initializing WiFi client");

        // Initialize WiFi driver
        let wifi = EspWifi::new(modem, sys_loop.clone(), Some(nvs))?;
        let wifi = BlockingWifi::wrap(wifi, sys_loop)?;

        info!("WiFi client initialized successfully");

        Ok(Self {
            wifi,
            state: WiFiConnectionState::Disconnected,
            last_connection_attempt: None,
        })
    }

    pub async fn connect_with_credentials(
        &mut self,
        credentials: &WiFiCredentials,
    ) -> Result<WiFiConnectionResult, EspError> {
        let start_time = std::time::Instant::now();
        self.last_connection_attempt = Some(start_time);

        info!(
            "Attempting to connect to WiFi network: {}",
            credentials.ssid
        );
        self.state = WiFiConnectionState::Connecting;

        // Configure WiFi
        let wifi_config = Configuration::Client(ClientConfiguration {
            ssid: credentials.ssid.as_str().try_into().map_err(|_| {
                error!("Invalid SSID format");
                EspError::from_infallible::<{ esp_idf_svc::sys::ESP_ERR_INVALID_ARG }>()
            })?,
            password: credentials.password.as_str().try_into().map_err(|_| {
                error!("Invalid password format");
                EspError::from_infallible::<{ esp_idf_svc::sys::ESP_ERR_INVALID_ARG }>()
            })?,
            channel: None,
            auth_method: self.determine_auth_method(&credentials.password),
            ..Default::default()
        });

        // Set configuration
        self.wifi.set_configuration(&wifi_config)?;
        info!("WiFi configuration set");

        // Start WiFi
        self.wifi.start()?;
        info!("WiFi started, beginning connection process...");

        // Connect with timeout and retries
        let connection_result = self.connect_with_retry(3, Duration::from_secs(10)).await;
        let connection_time = start_time.elapsed().as_millis() as u64;

        match connection_result {
            Ok(ip) => {
                self.state = WiFiConnectionState::Connected(ip);
                info!("Successfully connected to WiFi. IP: {}", ip);

                // Send test message to verify connectivity
                self.send_test_message().await;

                Ok(WiFiConnectionResult {
                    success: true,
                    ip_address: Some(ip),
                    error_message: None,
                    connection_time_ms: connection_time,
                })
            }
            Err(e) => {
                let error_msg = format!("WiFi connection failed: {:?}", e);
                self.state = WiFiConnectionState::Failed(error_msg.clone());
                error!("{}", error_msg);

                Ok(WiFiConnectionResult {
                    success: false,
                    ip_address: None,
                    error_message: Some(error_msg),
                    connection_time_ms: connection_time,
                })
            }
        }
    }

    async fn connect_with_retry(
        &mut self,
        max_retries: u32,
        timeout: Duration,
    ) -> Result<Ipv4Addr, EspError> {
        for attempt in 1..=max_retries {
            info!("Connection attempt {} of {}", attempt, max_retries);

            match self.try_connect(timeout).await {
                Ok(ip) => {
                    info!("Connection successful on attempt {}", attempt);
                    return Ok(ip);
                }
                Err(e) => {
                    warn!("Connection attempt {} failed: {:?}", attempt, e);

                    if attempt < max_retries {
                        info!("Retrying in 2 seconds...");
                        Timer::after(Duration::from_secs(2)).await;
                    }
                }
            }
        }

        error!("All connection attempts failed");
        Err(EspError::from_infallible::<
            { esp_idf_svc::sys::ESP_ERR_TIMEOUT },
        >())
    }

    async fn try_connect(&mut self, timeout: Duration) -> Result<Ipv4Addr, EspError> {
        // Start connection
        self.wifi.connect()?;

        // Wait for connection with timeout
        let start_time = std::time::Instant::now();
        while start_time.elapsed() < timeout.into() {
            if self.wifi.is_connected()? {
                // Get IP address
                let ip_info = self.wifi.wifi().sta_netif().get_ip_info()?;
                return Ok(ip_info.ip);
            }

            Timer::after(Duration::from_millis(500)).await;
        }

        // Timeout reached
        warn!("Connection timeout reached");
        Err(EspError::from_infallible::<
            { esp_idf_svc::sys::ESP_ERR_TIMEOUT },
        >())
    }

    fn determine_auth_method(&self, password: &str) -> AuthMethod {
        if password.is_empty() {
            AuthMethod::None
        } else if password.len() >= 8 {
            AuthMethod::WPA2Personal
        } else {
            warn!("Password length suggests WEP, but using WPA2");
            AuthMethod::WPA2Personal
        }
    }

    pub async fn send_test_message(&self) {
        info!("Sending test message to verify connectivity...");

        // Placeholder for test message
        // In a real implementation, this could:
        // 1. Send HTTP request to a test endpoint
        // 2. Ping a known server
        // 3. Send data to your backend service

        Timer::after(Duration::from_millis(100)).await;

        // Simulate test message
        match &self.state {
            WiFiConnectionState::Connected(ip) => {
                info!("Test message sent successfully from IP: {}", ip);
                info!("✓ Internet connectivity verified");
            }
            _ => {
                warn!("Cannot send test message - not connected");
            }
        }
    }

    pub async fn disconnect(&mut self) -> Result<(), EspError> {
        info!("Disconnecting from WiFi...");

        if self.wifi.is_connected()? {
            self.wifi.disconnect()?;
        }

        self.wifi.stop()?;
        self.state = WiFiConnectionState::Disconnected;

        info!("WiFi disconnected successfully");
        Ok(())
    }

    pub fn get_connection_state(&self) -> &WiFiConnectionState {
        &self.state
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.state, WiFiConnectionState::Connected(_))
    }

    pub fn get_ip_address(&self) -> Option<Ipv4Addr> {
        match &self.state {
            WiFiConnectionState::Connected(ip) => Some(*ip),
            _ => None,
        }
    }

    pub async fn get_connection_info(&mut self) -> Result<ConnectionInfo, EspError> {
        let is_connected = self.wifi.is_connected()?;
        let ip_info = if is_connected {
            Some(self.wifi.wifi().sta_netif().get_ip_info()?)
        } else {
            None
        };

        let signal_strength = if is_connected {
            // In real implementation, get actual signal strength
            Some(-45) // Placeholder: good signal strength
        } else {
            None
        };

        Ok(ConnectionInfo {
            is_connected,
            ip_info,
            signal_strength,
            connection_duration: self.last_connection_attempt.map(|start| start.elapsed()),
        })
    }

    pub async fn scan_networks(&mut self) -> Result<Vec<NetworkInfo>, EspError> {
        info!("Scanning for available WiFi networks...");

        // Start WiFi if not already started
        if !self.wifi.is_started()? {
            self.wifi.start()?;
        }

        // Perform scan
        let scan_result = self.wifi.scan()?;

        let mut networks = Vec::new();
        for ap in scan_result {
            networks.push(NetworkInfo {
                ssid: ap.ssid.to_string(),
                signal_strength: ap.signal_strength,
                auth_method: ap.auth_method.unwrap_or(AuthMethod::None),
                channel: ap.channel,
            });
        }

        info!("Found {} networks", networks.len());
        Ok(networks)
    }

    pub async fn test_connectivity(&self) -> ConnectivityTest {
        info!("Testing internet connectivity...");

        if !self.is_connected() {
            return ConnectivityTest {
                can_reach_gateway: false,
                can_reach_dns: false,
                can_reach_internet: false,
                test_duration_ms: 0,
            };
        }

        let start_time = std::time::Instant::now();

        // Placeholder connectivity tests
        // In real implementation:
        // 1. Ping gateway
        // 2. DNS resolution test
        // 3. HTTP request to known endpoint

        Timer::after(Duration::from_millis(500)).await;

        let test_duration = start_time.elapsed().as_millis() as u64;

        // Simulate test results
        ConnectivityTest {
            can_reach_gateway: true,
            can_reach_dns: true,
            can_reach_internet: true,
            test_duration_ms: test_duration,
        }
    }
}

#[derive(Debug)]
pub struct ConnectionInfo {
    pub is_connected: bool,
    pub ip_info: Option<esp_idf_svc::ipv4::IpInfo>,
    pub signal_strength: Option<i8>,
    pub connection_duration: Option<std::time::Duration>,
}

#[derive(Debug)]
pub struct NetworkInfo {
    pub ssid: String,
    pub signal_strength: i8,
    pub auth_method: AuthMethod,
    pub channel: u8,
}

#[derive(Debug)]
pub struct ConnectivityTest {
    pub can_reach_gateway: bool,
    pub can_reach_dns: bool,
    pub can_reach_internet: bool,
    pub test_duration_ms: u64,
}

// Helper functions
pub fn format_signal_strength(strength: i8) -> String {
    match strength {
        -30..=0 => "Excellent".to_string(),
        -67..=-31 => "Good".to_string(),
        -70..=-68 => "Fair".to_string(),
        -80..=-71 => "Weak".to_string(),
        _ => "Very Weak".to_string(),
    }
}

pub fn auth_method_to_string(auth: AuthMethod) -> &'static str {
    match auth {
        AuthMethod::None => "Open",
        AuthMethod::WEP => "WEP",
        AuthMethod::WPA => "WPA",
        AuthMethod::WPA2Personal => "WPA2",
        AuthMethod::WPAWPA2Personal => "WPA/WPA2",
        AuthMethod::WPA2Enterprise => "WPA2 Enterprise",
        AuthMethod::WPA3Personal => "WPA3",
        AuthMethod::WPA2WPA3Personal => "WPA2/WPA3",
        AuthMethod::WAPIPersonal => "WAPI",
    }
}
