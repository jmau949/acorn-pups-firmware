# Development Guide

This guide explains how to develop, test, and debug the ESP32 firmware using the dual-firmware architecture.

## 📁 Repository Structure

```
├── firmware-prod/         # Production firmware (ESP32 hardware)
│   ├── src/main.rs        # Original production code 
│   ├── src/ble_server.rs  # Real BLE hardware calls
│   ├── src/wifi_storage.rs# Real NVS storage
│   ├── Cargo.toml         # ESP32 dependencies
│   ├── build.rs           # ESP-IDF build script
│   ├── sdkconfig.defaults # ESP32 hardware configuration
│   └── rust-toolchain.toml# ESP32 Rust toolchain

├── firmware-dev/          # Development firmware (ESP32 simulation)
│   ├── src/main.rs        # Simulated version with same logic flow
│   ├── Cargo.toml         # ESP32 dependencies (same as prod)
│   ├── build.rs           # ESP-IDF build script
│   ├── sdkconfig.defaults # Simulation-optimized configuration
│   ├── rust-toolchain.toml# ESP32 Rust toolchain
│   ├── wokwi.toml         # Wokwi simulator configuration
│   ├── diagram.json       # Wokwi hardware layout (ESP32 + LEDs)
│   └── .cargo/config.toml # ESP32 target configuration

├── shared/                # Shared data structures and types
│   ├── src/lib.rs         # Library entry point
│   ├── src/types.rs       # Common enums, structs, events
│   └── Cargo.toml         # Shared type definitions

└── docs/                  # Documentation
    ├── DEVELOPMENT.md     # This file
    ├── BLE_LIFECYCLE.md   # BLE state management
    └── flash.md           # Hardware flashing guide
```

## 🔄 Development Workflow

### 1. Logic Development & Testing

```bash
# 1. Develop/modify shared types
cd shared/
# Edit src/types.rs for new events, states, or data structures

# 2. Test logic in development firmware
cd ../firmware-dev/
cargo build --release    # ESP32 target build
cargo run               # Run in Wokwi simulator

# 3. Deploy to production hardware
cd ../firmware-prod/
cargo build --release
cargo run              # Flash to real ESP32
```

### 2. Typical Development Session

```bash
# Start development session
cd firmware-dev/

# Make code changes to simulate new feature
# Edit src/main.rs to modify simulation logic

# Build and test quickly
cargo build --release
# Debug output shows in terminal

# Once logic works, update production
cd ../firmware-prod/
# Apply same logical changes using real hardware calls
cargo build --release
cargo run  # Flash to ESP32
```

## 🧪 Simulation vs Production

### Development Firmware (ESP32 Simulation)
- **Target**: `xtensa-esp32-espidf` (ESP32 but simulated I/O)
- **Hardware**: Simulated BLE, WiFi, LEDs via Wokwi
- **Storage**: In-memory credential storage
- **Debugging**: Console logs + visual LED feedback
- **Speed**: Faster iteration, no hardware flashing

### Production Firmware (ESP32 Hardware)
- **Target**: `xtensa-esp32-espidf` (Real ESP32)
- **Hardware**: Real BLE radio, WiFi, GPIO pins
- **Storage**: Real NVS flash storage
- **Debugging**: Serial monitor + hardware LEDs
- **Speed**: Slower iteration, requires hardware flashing

## 📋 Key Simulation Mappings

| Production | Development | Purpose |
|------------|-------------|---------|
| `esp_idf_svc::wifi::EspWifi` | Simulated connection logic | WiFi connectivity testing |
| `esp_idf_svc::bt::*` | Simulated BLE advertising | BLE provisioning flow |
| `esp_idf_svc::nvs::*` | In-memory storage | Credential persistence |
| `PinDriver::set_high()` | Wokwi LED control | Visual status feedback |
| `esp_restart()` | Logged restart event | System restart behavior |



## 🚦 LED Status System

Both firmwares use identical LED status logic:

- 🔴 **Red LEDs**: System startup, errors, unknown states
- 🔵 **Blue LEDs**: BLE advertising (waiting for mobile app)
- 🟢 **Green LEDs**: Connected (BLE client or WiFi)

### Hardware Mapping
- **Production**: GPIO pins 2, 4, 5 → Physical LEDs
- **Development**: GPIO pins 2, 4, 5 → Wokwi visual LEDs

## 📡 Event-Driven Architecture

Both firmwares use identical event flow:

```rust
// System Events
SystemEvent::SystemStartup     // Boot sequence
SystemEvent::ProvisioningMode  // BLE mode activated
SystemEvent::WiFiMode          // WiFi mode activated
SystemEvent::SystemError       // Error conditions
SystemEvent::TaskTerminating   // Clean shutdown

// WiFi Events  
WiFiConnectionEvent::ConnectionAttempting    // Connecting...
WiFiConnectionEvent::ConnectionSuccessful    // Connected with IP
WiFiConnectionEvent::ConnectionFailed        // Connection error
WiFiConnectionEvent::CredentialsStored       // Saved to storage
WiFiConnectionEvent::CredentialsInvalid      // Invalid credentials
```

## 🔍 Debugging Techniques

### Development Debugging
```bash
cd firmware-dev/
cargo build --release 2>&1 | tee build.log  # Capture build output
cargo run 2>&1 | tee run.log                # Capture runtime logs

# Key debug outputs:
# 🧪 Starting Wokwi Simulation Mode
# 🔌 LEDs initialized (Red: GPIO2, Green: GPIO4, Blue: GPIO5)
# 📻 Simulating BLE advertising - waiting for mobile app...
# 🟢 LEDs set to GREEN - BLE client connected
```

### Production Debugging
```bash
cd firmware-prod/
cargo build --release
cargo run    # Monitor via espflash

# Key debug outputs:
# 🚀 Starting Production Mode
# 🔌 Hardware LEDs initialized
# 📻 BLE server advertising started
# 🟢 Hardware LEDs: GREEN - Client connected
```

## 🔧 Build Commands Reference

### Production Firmware
```bash
cd firmware-prod/
cargo build                    # Debug build
cargo build --release          # Release build  
cargo run                     # Flash to ESP32
cargo run --release           # Flash optimized build
```

### Development Firmware  
```bash
cd firmware-dev/
cargo build                    # Debug build (ESP32 target)
cargo build --release          # Release build (ESP32 target)
# Note: Runs in Wokwi, not directly executable
```

### Shared Library
```bash
cd shared/
cargo build                    # Build shared types
cargo test                     # Run type tests
```

### Workspace Operations
```bash
# From project root:
cargo build -p pup-prod        # Build production only
cargo build -p pup-shared      # Build shared only
# Note: pup-dev must be built from its own directory
```

## 🚨 Common Issues & Solutions

### Development Build Issues
```bash
# If ESP32 toolchain issues:
cd firmware-dev/
export IDF_PATH=/home/esp/pup/.embuild/espressif/esp-idf/v5.1.2
cargo clean
cargo build --release
```

### Production Flash Issues
```bash
# If flashing fails:
cd firmware-prod/
cargo clean
cargo build --release
# Check USB connection and permissions
cargo run
```

### Shared Library Changes
```bash
# After modifying shared/src/types.rs:
cd shared/ && cargo build      # Rebuild shared lib
cd ../firmware-dev/ && cargo build --release  # Rebuild dev
cd ../firmware-prod/ && cargo build --release # Rebuild prod
```

## 🎯 Development Best Practices

1. **Start with Development**: Test logic in `firmware-dev` first
2. **Mirror Changes**: Apply same logical changes to `firmware-prod`
3. **Test Edge Cases**: Ensure both handle failures identically  
4. **Use Shared Types**: All data structures go in `shared/src/types.rs`
5. **Maintain Event Parity**: Same events, same timing, same order
6. **Visual Debugging**: Use LED states to debug logic flow
7. **Log Everything**: Comprehensive logging in both versions

## 🔄 Release Process

1. **Develop**: Create feature in `firmware-dev`
2. **Test**: Verify behavior in Wokwi simulation
3. **Mirror**: Apply changes to `firmware-prod` 
4. **Validate**: Test on real ESP32 hardware
5. **Deploy**: Flash production firmware to devices

This development workflow ensures reliable, tested firmware releases while maintaining fast iteration cycles. 