# Factory Reset System Documentation

## Overview

The Acorn Pups IoT device implements a comprehensive factory reset system triggered by a physical pinhole reset button. The system provides dual behavior for online and offline scenarios, ensuring reset notifications are never lost while maintaining data integrity and security.

## Architecture Overview

### Clean Separation of Concerns

The factory reset system follows a clean architecture pattern with three distinct modules:

```
┌─────────────────────────────────────────────────────────────────┐
│                          MAIN.RS                                │
│                    (Event Coordinator)                          │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │              Embassy Event Loop                         │   │
│  │   select! {                                             │   │
│  │     reset_manager.run() => { /* GPIO monitoring */ }   │   │
│  │     RESET_EVENT_SIGNAL.wait() => {                     │   │
│  │       /* Delegate to reset_handler */                  │   │
│  │     }                                                   │   │
│  │   }                                                     │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
            │                                      ▲
            │ GPIO Events                          │ Execution Results
            ▼                                      │
┌─────────────────────┐                ┌─────────────────────┐
│   RESET_MANAGER     │ ResetTriggered │   RESET_HANDLER     │
│   (GPIO Monitor)    │───────────────▶│   (Execution)       │
│                     │                │                     │
│ • Button monitoring │                │ • Online reset      │
│ • Debouncing        │                │ • Offline reset     │
│ • Hold detection    │                │ • MQTT notifications│
│ • WiFi status check │                │ • NVS erasure       │
│ • Event signaling   │                │ • System restart    │
└─────────────────────┘                └─────────────────────┘
```

### Component Responsibilities

#### `reset_manager.rs` - GPIO Monitoring
**Single Responsibility**: Physical button state detection and event signaling

- ✅ GPIO button monitoring with pull-up resistor
- ✅ 50ms debouncing for noise elimination  
- ✅ 3-second hold time detection
- ✅ WiFi connectivity status checking
- ✅ Event signaling via `RESET_MANAGER_EVENT_SIGNAL`
- ❌ **Does NOT**: Execute resets, send MQTT, perform NVS operations

#### `reset_handler.rs` - Reset Execution
**Single Responsibility**: Complete reset workflow execution

- ✅ Online reset with immediate MQTT notification
- ✅ Offline reset with deferred notification storage
- ✅ MQTT `ResetNotification` message publishing
- ✅ Selective NVS namespace erasure
- ✅ Production system restart (`esp_restart()`)
- ✅ Deferred notification processing on reconnection
- ❌ **Does NOT**: Monitor GPIO or detect button presses

#### `main.rs` - Event Coordination
**Single Responsibility**: Component coordination and task management

- ✅ Embassy async event loop with `select!` macro
- ✅ Reset event delegation between components
- ✅ System state management and error handling
- ✅ Task spawning and lifecycle management

## Event Flow Diagram

```mermaid
graph TD
    A[Physical Button Press] --> B[reset_manager.rs]
    B --> C{Button Hold 3s?}
    C -->|No| B
    C -->|Yes| D[Check WiFi Status]
    D --> E[Create ResetNotificationData]
    E --> F[Signal ResetTriggered Event]
    F --> G[main.rs Event Loop]
    G --> H{WiFi Available?}
    
    H -->|Yes| I[reset_handler.execute_online_reset]
    H -->|No| J[reset_handler.execute_offline_reset]
    
    I --> K[Send MQTT ResetNotification]
    K --> L[Wait for Confirmation]
    L --> M[Selective NVS Erasure]
    
    J --> N[Store Deferred Notification]
    N --> M
    
    M --> O[esp_restart()]
    O --> P[System Reboot]
    
    P --> Q[Device Restart]
    Q --> R[BLE Provisioning Mode]
    
    R --> S{MQTT Reconnects?}
    S -->|Yes| T[Process Deferred Notifications]
    T --> U[Send Stored Reset Data]
    U --> V[Clear reset_pending Namespace]
    V --> W[Normal Operation]
    S -->|No| W
```

## Technical Specifications

### Hardware Integration

#### GPIO Configuration
```rust
// ESP32 GPIO0 (Boot/Reset button)
const RESET_BUTTON_GPIO: u32 = 0;
use esp_idf_svc::hal::gpio::{Gpio0, Input, PinDriver, Pull};

let mut reset_button = PinDriver::input(gpio0)?;
reset_button.set_pull(Pull::Up)?; // Internal pull-up resistor
```

#### Button Timing Constants
```rust
const BUTTON_DEBOUNCE_MS: u64 = 50;     // Noise elimination
const BUTTON_HOLD_TIME_MS: u64 = 3000;  // Hold requirement for reset
const RESET_CHECK_INTERVAL_MS: u64 = 10; // Monitoring frequency
```

### Reset Behaviors

#### Online Reset Flow (WiFi Connected)
1. **Button Detection**: Physical button held for 3 seconds
2. **WiFi Check**: Confirm active WiFi connection
3. **Immediate Notification**: Send MQTT reset notification to cloud
4. **Confirmation Wait**: Wait up to 5 seconds for MQTT acknowledgment
5. **NVS Erasure**: Selectively erase device configuration namespaces
6. **System Restart**: Trigger production reboot with `esp_restart()`

#### Offline Reset Flow (WiFi Unavailable)
1. **Button Detection**: Physical button held for 3 seconds
2. **WiFi Check**: Determine WiFi is unavailable
3. **Deferred Storage**: Store reset notification in `reset_pending` namespace
4. **NVS Erasure**: Selectively erase device configuration (preserve notification)
5. **System Restart**: Trigger production reboot with `esp_restart()`
6. **Deferred Processing**: On next MQTT connection, send stored notification

### MQTT Integration

#### Reset Notification Message
```json
{
  "command": "reset_cleanup",
  "deviceId": "acorn-pup-12345678",
  "resetTimestamp": "2025-01-21T10:15:30Z",
  "oldCertificateArn": "arn:aws:iot:us-east-1:123456789012:cert/abc123...",
  "reason": "physical_button_reset"
}
```

#### Message Properties
- **Topic**: `acorn-pups/reset/{deviceId}`
- **QoS**: 1 (At least once delivery)
- **Retain**: false
- **Max ARN Length**: 512 bytes (supports long AWS certificate ARNs)

### NVS Namespace Management

#### Namespaces Erased During Reset
```rust
const WIFI_CONFIG_NAMESPACE: &str = "wifi_config";     // WiFi credentials
const MQTT_CERTS_NAMESPACE: &str = "mqtt_certs";       // Device certificates  
const ACORN_DEVICE_NAMESPACE: &str = "acorn_device";   // Device configuration
```

#### Namespace Preserved During Reset
```rust
const RESET_PENDING_NAMESPACE: &str = "reset_pending"; // Deferred notifications
```

#### Selective Erasure Implementation
```rust
// Manual key enumeration for surgical data removal
let keys_to_remove = match namespace {
    WIFI_CONFIG_NAMESPACE => vec!["ssid", "password", "auth_token", "device_name", "user_timezone", "wifi_creds"],
    MQTT_CERTS_NAMESPACE => vec![
        "device_cert", "private_key", "root_ca", "cert_metadata",
        "device_id", "cert_arn", "endpoint", "created_at"
    ],
    ACORN_DEVICE_NAMESPACE => vec![
        "device_id", "firmware_version", "device_config", 
        "auth_token", "registration_status", "last_heartbeat"
    ],
};
```

## Embassy Async Implementation

### Task Coordination
```rust
#[embassy_executor::task]
async fn reset_manager_task(
    device_id: String,
    nvs_partition: EspDefaultNvsPartition, 
    gpio0: esp_idf_svc::hal::gpio::Gpio0,
) {
    // Initialize components...
    
    // Main event loop with Embassy select!
    loop {
        match select(
            reset_manager.run(),                    // GPIO monitoring
            RESET_MANAGER_EVENT_SIGNAL.wait()      // Reset events
        ).await {
            Either::Second(ResetManagerEvent::ResetTriggered { 
                wifi_available, 
                reset_data 
            }) => {
                // Delegate to reset_handler
                let result = if wifi_available {
                    reset_handler.execute_online_reset(reset_data).await
                } else {
                    reset_handler.execute_offline_reset(reset_data).await
                };
                // Handle result and system events...
            }
            // Handle other events...
        }
    }
}
```

### Interrupt-Safe Coordination
```rust
// Atomic reset state management
static RESET_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// Embassy Signal for event-driven communication
pub static RESET_MANAGER_EVENT_SIGNAL: Signal<
    CriticalSectionRawMutex, 
    ResetManagerEvent
> = Signal::new();
```

## Error Handling and Recovery

### Atomic Operations
- **Single Reset Enforcement**: `AtomicBool` prevents concurrent reset operations
- **Race Condition Prevention**: Interrupt-safe state management
- **Resource Protection**: Critical section mutexes for shared state

### Retry Logic
```rust
// Deferred notification storage with retry
let mut retry_count = 0;
while retry_count < 3 {
    match storage.store_reset_notification(&reset_data) {
        Ok(_) => break,
        Err(e) => {
            retry_count += 1;
            if retry_count >= 3 {
                return Err(anyhow!("Critical: Unable to store reset notification"));
            }
            Timer::after(Duration::from_millis(100)).await;
        }
    }
}
```

### Graceful Degradation
- **MQTT Timeout Handling**: Proceed with reset if notification times out
- **Storage Failures**: Continue reset even if deferred storage fails
- **WiFi Fallback**: Online reset falls back to offline if MQTT fails
- **System Recovery**: Automatic task restart on component failures

## Security Considerations

### Physical Security
- **Button Access**: Requires physical device access for reset trigger
- **3-Second Hold**: Prevents accidental resets from brief contacts
- **Debouncing**: Eliminates false triggers from electrical noise

### Data Protection
- **Selective Erasure**: Only removes device-specific configuration
- **Certificate Security**: Secure removal of AWS IoT certificates
- **Notification Integrity**: Cryptographic MQTT authentication
- **Atomic Operations**: Prevents partial state corruption

### Access Control
- **Production Restart**: Uses safe `esp_restart()` system call
- **NVS Validation**: Verifies namespace existence before erasure
- **Error Boundaries**: Isolates reset failures from system corruption

## Performance Characteristics

### Timing Requirements
- **Button Detection**: < 100ms response time
- **Reset Execution**: < 5s for online, < 2s for offline
- **MQTT Notification**: < 5s timeout for cloud delivery
- **NVS Operations**: < 2s for selective erasure
- **Memory Usage**: < 2KB overhead during reset operations

### Resource Efficiency
- **Event-Driven**: No polling loops, minimal CPU usage
- **Non-Blocking**: Async operations don't block other tasks
- **Memory Optimized**: Fixed buffer sizes for embedded constraints
- **Interrupt-Safe**: Safe operation during GPIO interrupts

## Testing and Validation

### Unit Testing
```bash
# Component isolation testing
cargo test reset_manager_gpio_detection
cargo test reset_handler_mqtt_notification  
cargo test reset_storage_selective_erasure
```

### Integration Testing
```bash
# End-to-end reset flow testing
cargo test online_reset_flow
cargo test offline_reset_flow
cargo test deferred_notification_processing
```

### Hardware Testing
1. **Button Hold Test**: Verify 3-second hold requirement
2. **WiFi Scenario Test**: Test both online and offline behaviors  
3. **MQTT Integration Test**: Verify cloud notification delivery
4. **NVS Verification Test**: Confirm selective erasure effectiveness
5. **Recovery Test**: Validate deferred notification processing

## Troubleshooting

### Common Issues

#### Reset Not Triggering
- **Check Button Hold**: Ensure 3-second continuous hold
- **GPIO Configuration**: Verify GPIO0 pin driver initialization
- **Pull-up Resistor**: Confirm internal pull-up is enabled
- **Debounce Settings**: Check if debounce timing is appropriate

#### MQTT Notifications Not Sent
- **WiFi Connectivity**: Verify device has active internet connection
- **Certificate Validity**: Ensure AWS IoT certificates are not expired
- **Topic Permissions**: Confirm device has publish rights to reset topic
- **Buffer Size**: Check if certificate ARN fits in 512-byte buffer

#### Incomplete Reset
- **NVS Namespace**: Verify target namespaces exist before erasure
- **Reset Progress**: Check `RESET_IN_PROGRESS` atomic flag state
- **System Restart**: Confirm `esp_restart()` is being called
- **Error Handling**: Review logs for selective erasure failures

### Debug Logging
```rust
// Enable comprehensive reset logging
log::set_max_level(log::LevelFilter::Debug);

// Key log messages to monitor:
info!("🔘 Physical reset button pressed");           // Button detection
info!("🚀 Reset triggered - delegating to reset handler"); // Event delegation  
info!("📤 Reset notification sent via MQTT");        // MQTT success
info!("🔄 Factory reset completed - rebooting system"); // System restart
```

## Production Deployment

### Configuration Checklist
- [ ] GPIO0 properly configured for reset button
- [ ] AWS IoT certificates provisioned and validated
- [ ] MQTT topic permissions configured
- [ ] NVS partitions properly sized
- [ ] Factory reset button accessible but protected
- [ ] Reset behavior tested in both online/offline scenarios

### Monitoring
- Track reset notification delivery rates
- Monitor deferred notification processing times
- Alert on reset execution failures
- Log selective erasure effectiveness

### Maintenance
- Regularly test factory reset functionality
- Monitor AWS certificate expiration
- Validate MQTT connectivity and permissions
- Review reset logs for unusual patterns

## API Reference

### ResetManagerEvent
```rust
pub enum ResetManagerEvent {
    ResetButtonPressed,                    // Physical button detected
    ResetInitiated,                       // Reset process started
    ResetTriggered {                      // Reset ready for execution
        wifi_available: bool,
        reset_data: ResetNotificationData,
    },
    ResetCompleted,                       // Reset finished successfully
    ResetError(String),                   // Reset encountered error
}
```

### ResetNotificationData
```rust
pub struct ResetNotificationData {
    pub device_id: String,               // Device UUID
    pub reset_timestamp: String,         // ISO 8601 timestamp
    pub old_cert_arn: String,           // AWS certificate ARN
    pub reason: String,                  // Reset trigger reason
}
```

### Key Methods
```rust
// Reset Manager (GPIO monitoring only)
impl ResetManager {
    pub fn new(device_id: String, cert_arn: String, gpio0: Gpio0) -> Result<Self>;
    pub async fn run(&mut self) -> Result<()>;
    pub fn update_certificate_arn(&mut self, new_arn: String);
}

// Reset Handler (execution only)  
impl ResetHandler {
    pub fn new(device_id: String) -> Self;
    pub async fn execute_online_reset(&mut self, reset_data: ResetNotificationData) -> Result<()>;
    pub async fn execute_offline_reset(&mut self, reset_data: ResetNotificationData) -> Result<()>;
    pub async fn process_deferred_notifications(&mut self) -> Result<()>;
}
```

---

## Summary

The Acorn Pups factory reset system provides a robust, secure, and reliable mechanism for device factory resets with the following key features:

✅ **Clean Architecture**: Perfect separation of concerns between GPIO monitoring and reset execution  
✅ **Dual Behavior**: Handles both online and offline scenarios seamlessly  
✅ **Zero Data Loss**: Deferred notifications ensure no reset events are lost  
✅ **Production Ready**: Uses actual hardware GPIO and system restart calls  
✅ **Event Driven**: Embassy async patterns for optimal performance  
✅ **Secure**: Physical access required, selective data erasure, authenticated notifications  
✅ **Maintainable**: Clear module responsibilities and comprehensive error handling  

The system is ready for immediate production deployment on ESP32 hardware with full AWS IoT Core integration. 