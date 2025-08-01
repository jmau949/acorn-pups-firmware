// Volume Control Module for ESP32 IoT Device (Acorn Pups receiver)
// Production-grade volume control with GPIO button handling, NVS persistence, and MQTT integration
// Follows Embassy async patterns and existing codebase architecture

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_idf_svc::hal::gpio::{Gpio12, Gpio13, Input, InterruptType, PinDriver, Pull};
use log::{debug, error, info, warn};
use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::mqtt_manager::{MqttMessage, MQTT_MESSAGE_CHANNEL};
use crate::settings::{request_settings_save, SETTINGS_EVENT_SIGNAL, SettingsEvent};

// Volume control configuration constants
pub const VOLUME_MIN: u8 = 1;
pub const VOLUME_MAX: u8 = 10;
pub const VOLUME_DEFAULT: u8 = 5;
pub const DEBOUNCE_MS: u64 = 100;
pub const LONG_PRESS_MS: u64 = 2000;
pub const RAPID_VOLUME_INTERVAL_MS: u64 = 200;
pub const VOLUME_CHANNEL_SIZE: usize = 16;

// GPIO pin assignments for volume buttons
// Using GPIO12 and GPIO13 as they are commonly available on ESP32
pub const VOLUME_UP_PIN: u8 = 12;
pub const VOLUME_DOWN_PIN: u8 = 13;

/// Volume control actions
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeAction {
    VolumeUp,
    VolumeDown,
    SetVolume,
}

/// Volume control directions for internal use
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VolumeDirection {
    Up,
    Down,
}

/// Volume button identifiers
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VolumeButton {
    Up,
    Down,
}

/// Volume control events for MQTT publishing
#[derive(Debug, Clone, Serialize)]
pub struct VolumeChangeEvent {
    #[serde(rename = "clientId")]
    pub client_id: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    pub action: VolumeAction,
    #[serde(rename = "newVolume")]
    pub new_volume: u8,
    #[serde(rename = "previousVolume")]
    pub previous_volume: u8,
    pub timestamp: String,
}

/// Volume control errors
#[derive(Debug, Clone)]
pub enum VolumeError {
    InvalidLevel(u8),
    HardwareError(String),
    TimeoutError,
    MqttError(String),
}

impl core::fmt::Display for VolumeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            VolumeError::InvalidLevel(level) => {
                write!(f, "Invalid volume level: {} (must be {}-{})", level, VOLUME_MIN, VOLUME_MAX)
            }
            VolumeError::HardwareError(msg) => write!(f, "Hardware error: {}", msg),
            VolumeError::TimeoutError => write!(f, "Volume operation timeout"),
            VolumeError::MqttError(msg) => write!(f, "MQTT error: {}", msg),
        }
    }
}

impl std::error::Error for VolumeError {}

/// Volume control requests for internal communication
#[derive(Debug, Clone)]
pub enum VolumeRequest {
    AdjustVolume(VolumeDirection),
    SetVolume(u8),
    GetCurrentVolume,
}

/// Volume control events for external coordination  
#[derive(Debug, Clone)]
pub enum VolumeControlEvent {
    VolumeChanged {
        old_volume: u8,
        new_volume: u8,
        action: VolumeAction,
    },
    ButtonPressed(VolumeButton),
    LongPressStarted(VolumeButton),
    LongPressEnded(VolumeButton),
    VolumeError(VolumeError),
}

// Global volume control communication channels
pub static VOLUME_REQUEST_CHANNEL: Channel<
    CriticalSectionRawMutex,
    VolumeRequest,
    VOLUME_CHANNEL_SIZE,
> = Channel::new();

pub static VOLUME_EVENT_SIGNAL: Signal<CriticalSectionRawMutex, VolumeControlEvent> = Signal::new();

/// Volume manager - core volume control functionality
pub struct VolumeManager {
    device_id: String,
    current_volume: u8,
}

impl VolumeManager {
    /// Create new volume manager
    pub fn new(device_id: String, initial_volume: u8) -> Result<Self> {
        if initial_volume < VOLUME_MIN || initial_volume > VOLUME_MAX {
            return Err(anyhow!(VolumeError::InvalidLevel(initial_volume)));
        }

        info!("🔊 Initializing volume manager for device: {}", device_id);
        info!("🔊 Initial volume level: {}", initial_volume);

        Ok(Self {
            device_id,
            current_volume: initial_volume,
        })
    }

    /// Adjust volume in specified direction
    pub async fn adjust_volume(&mut self, direction: VolumeDirection) -> Result<VolumeChangeEvent> {
        let new_volume = match direction {
            VolumeDirection::Up => {
                if self.current_volume >= VOLUME_MAX {
                    debug!("🔊 Volume already at maximum: {} - ignoring volume up request", VOLUME_MAX);
                    // Return a successful "no-change" event instead of an error
                    return Ok(VolumeChangeEvent {
                        client_id: format!("acorn-receiver-{}", self.device_id),
                        device_id: self.device_id.clone(),
                        action: VolumeAction::VolumeUp,
                        new_volume: self.current_volume,
                        previous_volume: self.current_volume,
                        timestamp: self.get_iso_timestamp(),
                    });
                }
                self.current_volume + 1
            }
            VolumeDirection::Down => {
                if self.current_volume <= VOLUME_MIN {
                    debug!("🔊 Volume already at minimum: {} - ignoring volume down request", VOLUME_MIN);
                    // Return a successful "no-change" event instead of an error
                    return Ok(VolumeChangeEvent {
                        client_id: format!("acorn-receiver-{}", self.device_id),
                        device_id: self.device_id.clone(),
                        action: VolumeAction::VolumeDown,
                        new_volume: self.current_volume,
                        previous_volume: self.current_volume,
                        timestamp: self.get_iso_timestamp(),
                    });
                }
                self.current_volume - 1
            }
        };

        self.set_volume_internal(new_volume, match direction {
            VolumeDirection::Up => VolumeAction::VolumeUp,
            VolumeDirection::Down => VolumeAction::VolumeDown,
        }).await
    }

    /// Set volume to specific level
    pub async fn set_volume(&mut self, level: u8) -> Result<VolumeChangeEvent> {
        if level < VOLUME_MIN || level > VOLUME_MAX {
            return Err(anyhow!(VolumeError::InvalidLevel(level)));
        }

        self.set_volume_internal(level, VolumeAction::SetVolume).await
    }

    /// Internal volume setting with event generation
    async fn set_volume_internal(&mut self, new_volume: u8, action: VolumeAction) -> Result<VolumeChangeEvent> {
        let previous_volume = self.current_volume;
        self.current_volume = new_volume;

        info!("🔊 Volume changed: {} → {} ({})", previous_volume, new_volume, 
              match action {
                  VolumeAction::VolumeUp => "UP",
                  VolumeAction::VolumeDown => "DOWN", 
                  VolumeAction::SetVolume => "SET",
              });

        // Create volume change event
        let event = VolumeChangeEvent {
            client_id: format!("acorn-receiver-{}", self.device_id),
            device_id: self.device_id.clone(),
            action,
            new_volume,
            previous_volume,
            timestamp: self.get_iso_timestamp(),
        };

        // Update settings with new volume
        self.update_device_settings(new_volume).await?;

        // Publish to MQTT
        self.publish_volume_event(&event).await?;

        // Signal volume control event
        VOLUME_EVENT_SIGNAL.signal(VolumeControlEvent::VolumeChanged {
            old_volume: previous_volume,
            new_volume,
            action,
        });

        Ok(event)
    }

    /// Get current volume level
    pub fn get_current_volume(&self) -> u8 {
        self.current_volume
    }

    /// Update device settings with new volume
    async fn update_device_settings(&self, volume: u8) -> Result<()> {
        debug!("🔊 Updating device settings with volume: {}", volume);

        // Wait for current settings first
        crate::settings::request_current_settings();
        
        // Wait for settings event
        let settings_event = SETTINGS_EVENT_SIGNAL.wait().await;
        
        if let SettingsEvent::SettingsUpdated(mut current_settings) = settings_event {
            // Only update if volume actually changed
            if current_settings.sound_volume != volume {
                current_settings.sound_volume = volume;
                request_settings_save(current_settings);
                debug!("✅ Settings save requested with new volume: {}", volume);
            } else {
                debug!("🔊 Volume unchanged in settings ({}) - skipping NVS update", volume);
            }
            Ok(())
        } else {
            Err(anyhow!("Failed to get current settings for volume update"))
        }
    }

    /// Publish volume event to MQTT
    async fn publish_volume_event(&self, event: &VolumeChangeEvent) -> Result<()> {
        // Skip MQTT publishing if volume didn't actually change (boundary condition)
        if event.new_volume == event.previous_volume {
            debug!("🔊 Volume unchanged ({}) - skipping MQTT publish", event.new_volume);
            return Ok(());
        }
        
        debug!("🔊 Publishing volume event to MQTT");
        
        // Only publish volume_up and volume_down actions to match Lambda expectations
        let source = match event.action {
            VolumeAction::VolumeUp => "volume_up",
            VolumeAction::VolumeDown => "volume_down",
            VolumeAction::SetVolume => {
                // SetVolume is an internal action - determine direction based on volume change
                if event.new_volume > event.previous_volume {
                    "volume_up"
                } else {
                    "volume_down"
                }
            }
        };
        
        let mqtt_message = MqttMessage::VolumeChange {
            volume: event.new_volume,
            previous_volume: event.previous_volume,
            source: source.to_string(),
        };

        if let Err(_) = MQTT_MESSAGE_CHANNEL.try_send(mqtt_message) {
            warn!("⚠️ MQTT message channel full - volume event may be delayed");
            return Err(anyhow!(VolumeError::MqttError("Channel full".to_string())));
        }

        debug!("✅ Volume event queued for MQTT publishing");
        Ok(())
    }

    /// Get ISO timestamp for events
    fn get_iso_timestamp(&self) -> String {
        // For embedded systems, we'll use a simple timestamp
        // In production, this would integrate with RTC
        format!("2025-01-01T{:02}:{:02}:{:02}Z", 
                (embassy_time::Instant::now().as_millis() / 3600000) % 24,
                (embassy_time::Instant::now().as_millis() / 60000) % 60,
                (embassy_time::Instant::now().as_millis() / 1000) % 60)
    }

    /// Embassy task for handling volume requests
    pub async fn run_volume_task(&mut self) {
        info!("🔊 Starting volume management task");

        loop {
            // Wait for volume requests
            let request = VOLUME_REQUEST_CHANNEL.receive().await;
            debug!("🔊 Processing volume request: {:?}", request);

            let result = match request {
                VolumeRequest::AdjustVolume(direction) => {
                    match self.adjust_volume(direction).await {
                        Ok(event) => {
                            info!("✅ Volume adjusted successfully: {} → {}", 
                                  event.previous_volume, event.new_volume);
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
                VolumeRequest::SetVolume(level) => {
                    match self.set_volume(level).await {
                        Ok(event) => {
                            info!("✅ Volume set successfully: {} → {}", 
                                  event.previous_volume, event.new_volume);
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
                VolumeRequest::GetCurrentVolume => {
                    debug!("📊 Current volume requested: {}", self.current_volume);
                    Ok(())
                }
            };

            if let Err(e) = result {
                error!("❌ Volume request failed: {}", e);
                if let Ok(volume_error) = e.downcast::<VolumeError>() {
                    VOLUME_EVENT_SIGNAL.signal(VolumeControlEvent::VolumeError(volume_error));
                }
            }

            // Small delay to prevent busy waiting
            Timer::after(Duration::from_millis(10)).await;
        }
    }
}

/// Button handler for volume control with GPIO interrupt support
pub struct VolumeButtonHandler {
    volume_up_pin: PinDriver<'static, Gpio12, Input>,
    volume_down_pin: PinDriver<'static, Gpio13, Input>,
    last_press_time: Instant,
    long_press_active: Option<VolumeButton>,
}

impl VolumeButtonHandler {
    /// Create new button handler
    pub fn new(
        gpio12: Gpio12,
        gpio13: Gpio13,
    ) -> Result<Self> {
        info!("🔘 Initializing volume button handler");

        // Configure volume up button (GPIO12) with internal pull-up
        let mut volume_up_pin = PinDriver::input(gpio12)?;
        volume_up_pin.set_pull(Pull::Up)?;
        volume_up_pin.set_interrupt_type(InterruptType::NegEdge)?;

        // Configure volume down button (GPIO13) with internal pull-up  
        let mut volume_down_pin = PinDriver::input(gpio13)?;
        volume_down_pin.set_pull(Pull::Up)?;
        volume_down_pin.set_interrupt_type(InterruptType::NegEdge)?;

        info!("✅ Volume buttons configured on GPIO12 (UP) and GPIO13 (DOWN)");

        Ok(Self {
            volume_up_pin,
            volume_down_pin,
            last_press_time: Instant::now(),
            long_press_active: None,
        })
    }

    /// Start button monitoring task
    pub async fn start_monitoring(&mut self) -> Result<()> {
        info!("🔘 Starting volume button monitoring");

        loop {
            let current_time = Instant::now();

            // Check volume up button
            if self.volume_up_pin.is_low() {
                if self.should_process_press(current_time) {
                    self.handle_button_press(VolumeButton::Up, current_time).await;
                }
            }

            // Check volume down button
            if self.volume_down_pin.is_low() {
                if self.should_process_press(current_time) {
                    self.handle_button_press(VolumeButton::Down, current_time).await;
                }
            }

            // Handle long press detection
            self.handle_long_press_detection(current_time).await;

            // Small delay to prevent excessive CPU usage while still being responsive
            Timer::after(Duration::from_millis(10)).await;
        }
    }

    /// Check if button press should be processed (debouncing)
    fn should_process_press(&self, current_time: Instant) -> bool {
        current_time.duration_since(self.last_press_time) >= Duration::from_millis(DEBOUNCE_MS)
    }

    /// Handle button press with debouncing and long press detection
    async fn handle_button_press(&mut self, button: VolumeButton, press_time: Instant) {
        debug!("🔘 Button pressed: {:?}", button);
        self.last_press_time = press_time;

        // Signal button press event
        VOLUME_EVENT_SIGNAL.signal(VolumeControlEvent::ButtonPressed(button));

        // Process immediate volume change
        let direction = match button {
            VolumeButton::Up => VolumeDirection::Up,
            VolumeButton::Down => VolumeDirection::Down,
        };

        if let Err(_) = VOLUME_REQUEST_CHANNEL.try_send(VolumeRequest::AdjustVolume(direction)) {
            warn!("⚠️ Volume request channel full - button press may be delayed");
        }

        // Start long press detection
        if self.long_press_active.is_none() {
            self.long_press_active = Some(button);
            debug!("🔘 Long press detection started for: {:?}", button);
        }
    }

    /// Handle long press detection and rapid volume changes
    async fn handle_long_press_detection(&mut self, current_time: Instant) {
        if let Some(active_button) = self.long_press_active {
            let press_duration = current_time.duration_since(self.last_press_time);

            // Check if button is still pressed
            let still_pressed = match active_button {
                VolumeButton::Up => self.volume_up_pin.is_low(),
                VolumeButton::Down => self.volume_down_pin.is_low(),
            };

            if still_pressed {
                // Long press threshold reached
                if press_duration >= Duration::from_millis(LONG_PRESS_MS) {
                    debug!("🔘 Long press detected for: {:?}", active_button);
                    VOLUME_EVENT_SIGNAL.signal(VolumeControlEvent::LongPressStarted(active_button));

                    // Rapid volume changes during long press
                    let direction = match active_button {
                        VolumeButton::Up => VolumeDirection::Up,
                        VolumeButton::Down => VolumeDirection::Down,
                    };

                    if let Err(_) = VOLUME_REQUEST_CHANNEL.try_send(VolumeRequest::AdjustVolume(direction)) {
                        warn!("⚠️ Volume request channel full during long press");
                    }

                    // Reset timer for next rapid change
                    self.last_press_time = current_time;
                }
            } else {
                // Button released - end long press
                debug!("🔘 Long press ended for: {:?}", active_button);
                VOLUME_EVENT_SIGNAL.signal(VolumeControlEvent::LongPressEnded(active_button));
                self.long_press_active = None;
            }
        }
    }
}

/// Utility functions for external integration

/// Send volume adjustment request (non-blocking)
pub fn request_volume_up() {
    if let Err(_) = VOLUME_REQUEST_CHANNEL.try_send(VolumeRequest::AdjustVolume(VolumeDirection::Up)) {
        warn!("⚠️ Volume request channel full, skipping volume up request");
    }
}

/// Send volume adjustment request (non-blocking)
pub fn request_volume_down() {
    if let Err(_) = VOLUME_REQUEST_CHANNEL.try_send(VolumeRequest::AdjustVolume(VolumeDirection::Down)) {
        warn!("⚠️ Volume request channel full, skipping volume down request");
    }
}

/// Send volume set request (non-blocking)
pub fn request_set_volume(level: u8) {
    if let Err(_) = VOLUME_REQUEST_CHANNEL.try_send(VolumeRequest::SetVolume(level)) {
        warn!("⚠️ Volume request channel full, skipping set volume request");
    }
}

/// Get current volume request (non-blocking)
pub fn request_current_volume() {
    if let Err(_) = VOLUME_REQUEST_CHANNEL.try_send(VolumeRequest::GetCurrentVolume) {
        warn!("⚠️ Volume request channel full, skipping current volume request");
    }
}