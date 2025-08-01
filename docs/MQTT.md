# MQTT Async Architecture Documentation

## Overview

This document explains the async MQTT implementation using Embassy runtime for ESP32 firmware. The system provides event-driven, non-blocking MQTT communication with AWS IoT Core through a carefully orchestrated async architecture that prevents deadlocks and ensures continuous event processing.

## Table of Contents

1. [Architecture Components](#architecture-components)
2. [Embassy Async Runtime](#embassy-async-runtime)  
3. [Event Processing System](#event-processing-system)
4. [Connection Management](#connection-management)
5. [Subscription Handling](#subscription-handling)
6. [Message Flow](#message-flow)
7. [Blocking vs Non-blocking Operations](#blocking-vs-non-blocking-operations)
8. [File Structure and Responsibilities](#file-structure-and-responsibilities)

## Architecture Components

### Core Components

**MqttManager**
- Primary orchestrator for MQTT operations
- Manages Embassy task coordination and message queuing
- Handles connection lifecycle and health monitoring
- Coordinates between different async subsystems

**AwsIotMqttClient** 
- Low-level MQTT client wrapper around ESP-IDF's `EspAsyncMqttClient`
- Handles connection establishment and event processing
- Manages subscription requests and message publishing
- Provides async interface for MQTT operations

**MqttCertificateStorage**
- Manages X.509 certificate storage and retrieval
- Handles certificate validation and lifecycle
- Provides secure NVS-based certificate persistence

### Data Structures

**Connection State Management**
```rust
pub enum MqttConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}
```

**Message Queue System**
```rust
pub enum MqttMessage {
    ButtonPress { button_rf_id: String, battery_level: Option<u8> },
    DeviceStatus { status: String, wifi_signal: Option<i32> },
    VolumeChange { volume: u8, source: String },
    Connect,
    Disconnect,
    ForceReconnect,
}
```

**Event Communication**
```rust
pub enum MqttManagerEvent {
    Connected,
    Disconnected,
    ConnectionError(String),
    MessagePublished(String),
    MessageFailed(String),
    CertificatesLoaded,
    CertificatesError(String),
}
```

### Embassy Communication Channels

**MQTT_MESSAGE_CHANNEL**
- Embassy Channel for queuing outbound MQTT messages
- Provides backpressure management with configurable queue size
- Enables non-blocking message submission from other tasks

**MQTT_EVENT_SIGNAL**
- Embassy Signal for broadcasting MQTT manager events
- Allows other system components to react to MQTT state changes
- Single-value signal that overwrites previous values

## Embassy Async Runtime

### Task Orchestration

The Embassy runtime provides cooperative multitasking through Rust's async/await mechanism. Key characteristics:

**Cooperative Scheduling**
- Tasks voluntarily yield control at `.await` points
- No preemptive multitasking or context switching overhead
- Deterministic execution order based on event availability

**Memory Efficiency**
- Tasks are zero-cost abstractions until awaited
- No separate thread stacks - futures stored on heap
- Minimal runtime overhead compared to RTOS threads

**Event-Driven Execution**
- Tasks suspend until events become available
- Executor polls futures only when events occur
- Efficient power management through idle states

### Async Patterns Used

**select! Operations**
```rust
match embassy_futures::select::select(
    MQTT_MESSAGE_CHANNEL.receive(),
    self.client.process_messages()
).await {
    Either::First(message) => { /* Handle queued message */ }
    Either::Second(result) => { /* Handle MQTT event */ }
}
```

**Timer-based Operations**
```rust
embassy_time::Timer::after(Duration::from_millis(500)).await;
```

**Channel Communication**
```rust
MQTT_MESSAGE_CHANNEL.send(MqttMessage::Connect).await;
let message = MQTT_MESSAGE_CHANNEL.receive().await;
```

## Event Processing System

### Event Loop Architecture

The MQTT manager runs a continuous event loop that handles multiple event sources:

1. **Message Queue Processing** - Handles outbound messages from `MQTT_MESSAGE_CHANNEL`
2. **MQTT Event Processing** - Processes incoming MQTT protocol events
3. **Health Monitoring** - Periodic connection health checks
4. **Subscription Management** - Manages topic subscriptions and confirmations

### Event Flow

**Outbound Message Flow**
1. Application components send messages to `MQTT_MESSAGE_CHANNEL`
2. MqttManager receives messages in main event loop
3. Messages are converted to appropriate MQTT publish operations
4. ESP-IDF MQTT client sends messages to AWS IoT Core
5. Publish confirmations are processed through event system

**Inbound Message Flow**
1. AWS IoT Core sends messages to subscribed topics
2. ESP-IDF MQTT client receives messages via TLS connection
3. Messages are queued in ESP-IDF's internal event system
4. MQTT client's `process_messages()` extracts events asynchronously
5. Events are routed to appropriate handlers based on topic patterns
6. Application logic processes the received messages

### Event Processing Methods

**process_messages()**
- Extracts one MQTT event from ESP-IDF's internal queue
- Calls both connection event handler and async message handler
- Returns immediately if no events are available
- Must be called continuously to maintain connection health

**handle_connection_event()**
- Processes connection-related events (Connected, Disconnected, Error)
- Updates internal connection state
- Handles subscription confirmations and error conditions
- Synchronous event processing for state management

**handle_mqtt_event_async()**
- Processes message-related events asynchronously
- Routes messages to topic-specific handlers
- Handles message parsing and application logic
- Asynchronous processing allows for complex operations

## Connection Management

### Connection Lifecycle

**Initialization Phase**
1. Load X.509 certificates from NVS storage
2. Create ESP-IDF MQTT client configuration with certificates
3. Set connection parameters (keep-alive, timeouts, QoS levels)

**Connection Phase**
1. Initiate TLS handshake with AWS IoT Core
2. Perform mutual certificate authentication
3. Establish encrypted MQTT connection
4. Update connection state to Connected

**Operational Phase**
1. Begin continuous event processing loop
2. Start subscription process for device-specific topics
3. Enable message publishing and receiving
4. Monitor connection health periodically

**Disconnection Handling**
1. Detect disconnection through event processing
2. Update connection state and notify other components
3. Attempt automatic reconnection with exponential backoff
4. Re-subscribe to topics after successful reconnection

### Keep-Alive Mechanism

The MQTT protocol requires periodic keep-alive messages to maintain connections:

**Keep-Alive Configuration**
- Interval set to 60 seconds in MQTT client configuration
- ESP-IDF automatically sends PINGREQ messages
- AWS IoT Core responds with PINGRESP messages

**Keep-Alive Processing**
- Keep-alive messages are processed in the event loop
- Continuous `process_messages()` calls ensure timely processing
- Connection timeout occurs if keep-alive messages are not processed
- Event loop must never block to prevent timeout disconnections

## Subscription Handling

### Subscription Topics

The device subscribes to three device-specific topics:

1. **Settings Topic** (`acorn-pups/settings/{client-id}`): Device configuration updates
2. **Status Request Topic** (`acorn-pups/status-request/{client-id}`): Status polling requests
3. **Commands Topic** (`acorn-pups/commands/{client-id}`): Device control commands

### Subscription Challenge

The original issue was caused by ESP-IDF's internal subscription queuing behavior:

**Problem**: The ESP-IDF MQTT client queues subscription requests internally and only processes them when the event loop runs. Waiting for subscription confirmations in a loop prevents the event loop from processing the queued requests.

**Solution**: Use a fire-and-forget approach that submits all subscription requests without waiting for confirmations:

### Fire-and-Forget Subscription Pattern

```rust
// Submit all subscription requests without waiting
for (topic_base, topic_name) in &topics {
    let topic = format!("{}/{}", topic_base, client_id);
    match client.subscribe(&topic, QoS::AtLeastOnce).await {
        Ok(message_id) => {
            info!("✅ {} subscription request queued - Message ID: {}", topic_name, message_id);
        }
        Err(e) => {
            error!("❌ Failed to queue {} subscription request: {:?}", topic_name, e);
        }
    }
    // Small delay between requests
    embassy_time::Timer::after(embassy_time::Duration::from_millis(100)).await;
}
```

**How It Works**
1. All subscription requests are submitted to ESP-IDF's internal queue
2. Function returns immediately without waiting for confirmations
3. ESP-IDF processes subscription requests when the event loop runs
4. `Subscribed` events are received and processed asynchronously
5. Connection remains healthy with continuous event processing

### Subscription Confirmation Flow

1. **Request Sent** - Subscription request sent to AWS IoT Core
2. **Event Processing Continues** - Event loop is not blocked
3. **Confirmation Received** - AWS IoT Core sends subscription confirmation
4. **Event Processed** - `Subscribed` event is processed in event loop
5. **Ready for Messages** - Topic is now active for receiving messages

## Message Flow

### Publishing Messages

**Async Publishing Process**
1. Application code calls `MQTT_MESSAGE_CHANNEL.send(message).await`
2. MqttManager receives message in main event loop
3. Message is converted to JSON and sent via `client.publish().await`
4. Publish acknowledgment is received through event processing
5. Success/failure is communicated via `MQTT_EVENT_SIGNAL`

### Receiving Messages

**Async Receiving Process**
1. AWS IoT Core publishes message to subscribed topic
2. ESP-IDF MQTT client receives message via TLS connection
3. Message is queued in ESP-IDF's internal event system
4. Next `process_messages()` call extracts the message event
5. Message is routed to appropriate handler based on topic
6. Application logic processes the message content


## File Structure and Responsibilities

### mqtt_client.rs

**Primary Responsibilities**
- Low-level MQTT protocol handling
- ESP-IDF `EspAsyncMqttClient` wrapper
- Connection state management
- Event processing coordination

**Key Components**
- `AwsIotMqttClient` struct with connection state
- `process_messages()` for continuous event processing
- `subscribe_to_device_topics()` for non-blocking subscriptions
- Event handlers for connection and message events

**Async Patterns**
- Embassy-compatible async methods
- Non-blocking subscription with `select!` pattern
- Continuous event processing loop
- Timer-based yielding for responsiveness

### mqtt_manager.rs

**Primary Responsibilities**
- High-level MQTT orchestration
- Embassy task coordination
- Message queue management
- Connection lifecycle management

**Key Components**
- `MqttManager` struct with client coordination
- Main event loop with `select!` for multiple event sources
- Message routing and processing
- Health monitoring and reconnection logic

**Async Patterns**
- Embassy channels for inter-task communication
- Embassy signals for event broadcasting
- Continuous async event loop
- Structured concurrency with `select!`

### mqtt_certificates.rs

**Primary Responsibilities**
- X.509 certificate storage and retrieval
- Certificate validation and lifecycle management
- NVS-based persistent storage
- Security and compliance enforcement

**Key Components**
- `MqttCertificateStorage` for certificate operations
- Dynamic buffer allocation for memory efficiency
- Certificate format validation
- Secure storage patterns

**Async Patterns**
- Async certificate loading operations
- Non-blocking NVS storage access
- Error handling with Result types
- Memory-efficient operations

### Integration Points

**Manager-Client Relationship**
- Manager owns and coordinates client lifecycle
- Manager handles high-level operations, client handles protocol
- Event communication through Embassy channels and signals
- Clean separation of concerns between orchestration and implementation

**Certificate-Client Integration**
- Client loads certificates through certificate storage
- Certificate validation occurs before connection attempts
- Certificate errors are propagated through async error handling
- Dynamic certificate updates supported through async reloading

**Embassy Runtime Integration** 
- All components use Embassy async primitives
- Cooperative scheduling ensures efficient resource usage
- Event-driven architecture prevents polling and busy waiting
- Structured concurrency patterns manage complex async operations

## Conclusion

The async MQTT architecture leverages Embassy's cooperative runtime to provide efficient, responsive MQTT communication. Key design principles include:

- **Non-blocking Operations**: All operations use timeouts or yield control regularly
- **Event-driven Processing**: Continuous event loops handle protocol requirements
- **Structured Concurrency**: Embassy patterns manage complex async coordination
- **Resource Efficiency**: Cooperative scheduling minimizes overhead
- **Connection Reliability**: Continuous event processing maintains protocol compliance

This architecture ensures reliable MQTT communication while maintaining system responsiveness and preventing the deadlocks that can occur with blocking async operations.