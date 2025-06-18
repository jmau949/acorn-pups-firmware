# ESP32 WiFi Provisioning with BLE - Beginner's Guide

This project demonstrates a complete WiFi provisioning system for ESP32 microcontrollers using Bluetooth Low Energy (BLE). It's designed to be educational for developers new to Rust and embedded programming.

## 🎯 What This Project Does

The ESP32 device starts up and checks if it knows how to connect to WiFi. If not, it becomes a Bluetooth device that mobile apps can connect to. The mobile app sends WiFi network credentials via Bluetooth, the ESP32 stores them permanently, connects to WiFi, and then turns off Bluetooth since it's no longer needed.

## 🏗️ System Architecture

```
Mobile App (phone/tablet)
    ↓ (Bluetooth LE)
ESP32 BLE Server → WiFi Storage (NVS Flash) → WiFi Connection → Internet
    ↑                                              ↓
LED Controller ←―――――――――――――――――――――――――――――――――――――┘
```

## 📁 Project Structure

```
src/
├── main.rs          # Main application with Embassy async runtime
├── ble_server.rs    # Bluetooth Low Energy advertising and communication
└── wifi_storage.rs  # Persistent storage in flash memory (NVS)
```

## 🔧 Core Technologies

### Embassy Async Runtime
- **What it is**: Like Node.js or Python asyncio, but for microcontrollers
- **Why we use it**: Allows multiple tasks to run "simultaneously" without blocking each other
- **Key concept**: `async`/`await` lets the processor work on other tasks while waiting for I/O

### ESP-IDF (ESP32 Development Framework)
- **What it is**: The official development framework for ESP32 chips
- **Language**: Written in C, but we use Rust bindings
- **Provides**: WiFi drivers, Bluetooth stack, hardware access, flash storage

### NVS (Non-Volatile Storage)
- **What it is**: Key-value database stored in flash memory
- **Survives**: Power loss, reboots, firmware updates
- **Use case**: Storing WiFi credentials permanently

## 📋 Detailed Code Explanation

### Main Application Flow (`main.rs`)

1. **System Initialization**
   ```rust
   esp_idf_svc::sys::link_patches();  // Connect Rust to ESP-IDF C libraries
   esp_idf_svc::log::EspLogger::initialize_default();  // Enable logging
   ```

2. **Hardware Setup**
   ```rust
   let peripherals = Peripherals::take().unwrap();  // Get exclusive hardware access
   let led_red = PinDriver::output(peripherals.pins.gpio2)?;  // Configure GPIO pin
   ```

3. **Task Spawning**
   ```rust
   spawner.spawn(ble_task())?;  // Start BLE provisioning task
   spawner.spawn(led_task(...))?;  // Start LED pattern task
   ```

### BLE Server (`ble_server.rs`)

#### Key Concepts:
- **UUID (Universally Unique Identifier)**: 128-bit numbers that identify BLE services
- **GATT (Generic Attribute Profile)**: How BLE devices expose data to clients
- **Characteristics**: Individual data points that mobile apps can read/write

#### Process Flow:
1. **Advertising**: Make device discoverable as "AcornPups-XXXX"
2. **Service Creation**: Set up WiFi provisioning service with UUIDs
3. **Data Reception**: Receive SSID and password from mobile app
4. **Validation**: Check that credentials are properly formatted
5. **Storage**: Save credentials to flash memory for future use

### WiFi Storage (`wifi_storage.rs`)

#### NVS Operations:
- **Namespace**: Groups related data (like "wifi_config")
- **Keys**: Individual data identifiers ("ssid", "password")
- **Serialization**: Convert Rust structs to JSON for storage

#### Data Persistence:
```rust
// Store credentials
self.nvs.set_str(SSID_KEY, &credentials.ssid)?;

// Load credentials
let ssid = self.nvs.get_str(SSID_KEY, &mut buffer)?;
```

### WiFi Connection (in `main.rs`)

#### Connection Process:
1. **Configuration**: Use stored network name (SSID) and password from NVS
2. **Authentication**: Simulated connection for demonstration purposes
3. **Association**: In production, would connect to actual access point using ESP-IDF
4. **DHCP**: Would get IP address from router in real implementation
5. **Verification**: Send test message to verify connectivity

## 🔄 Complete Workflow

### First Boot (No Stored WiFi)
1. Device starts up
2. Checks NVS for stored WiFi credentials → None found
3. Starts BLE advertising as "AcornPups-1234"
4. Mobile app connects via Bluetooth
5. App sends WiFi network name and password
6. Device validates and stores credentials in flash
7. Device connects to WiFi network
8. Device gets IP address (e.g., 192.168.1.100)
9. Device sends test message to verify internet
10. Device notifies mobile app of success
11. Device stops BLE advertising (no longer needed)

### Subsequent Boots (WiFi Stored)
1. Device starts up
2. Checks NVS for stored WiFi credentials → Found!
3. Automatically connects to stored WiFi network
4. Skips BLE provisioning entirely
5. Ready for normal operation

## 🎨 LED Patterns

The LED task demonstrates concurrent execution:

1. **Pattern 1**: Sequential (Red → Green → Blue)
2. **Pattern 2**: All LEDs together (white)
3. **Pattern 3**: Alternating pairs (purple ↔ green)
4. **Pattern 4**: Fast individual blinks

These patterns run independently of WiFi/BLE operations.

## 🔍 Key Rust Concepts for Beginners

### Ownership
```rust
let led_red = PinDriver::output(peripherals.pins.gpio2)?;
spawner.spawn(led_task(led_red, led_green, led_blue))?;
// led_red is "moved" into the task - main() can't use it anymore
```

### Error Handling
```rust
match PinDriver::output(peripherals.pins.gpio2) {
    Ok(pin) => pin,      // Success case
    Err(e) => {          // Error case
        warn!("Failed: {:?}", e);
        return;          // Exit early
    }
}
```

### Async/Await
```rust
Timer::after(Duration::from_millis(300)).await;  // Non-blocking delay
// Other tasks can run during this delay
```

### Pattern Matching
```rust
match pattern {
    0 => { /* Pattern 1 logic */ }
    1 => { /* Pattern 2 logic */ }
    _ => { /* Default case */ }
}
```

## 🛠️ Building and Running

### Prerequisites
```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install ESP-IDF and Rust ESP toolchain
cargo install espup
espup install
```

### Build Commands
```bash
# Check for compilation errors
cargo check

# Build the project
cargo build

# Build and flash to ESP32
cargo run
```

## 📱 Mobile App Integration

Your mobile app needs to:

1. **Scan for BLE devices** with name "AcornPups-XXXX"
2. **Connect to device** and discover services
3. **Find WiFi service** using UUID: `12345678-1234-1234-1234-123456789abc`
4. **Write SSID** to characteristic: `12345678-1234-1234-1234-123456789abd`
5. **Write password** to characteristic: `12345678-1234-1234-1234-123456789abe`
6. **Read status** from characteristic: `12345678-1234-1234-1234-123456789abf`

## 🚀 Next Steps for Production

This is a learning framework. For production use, you'd need to:

1. **Real BLE Stack**: Replace placeholder BLE code with actual ESP-IDF Bluetooth
2. **Security**: Encrypt credentials during BLE transmission
3. **Error Recovery**: Handle network failures, credential corruption, etc.
4. **Power Management**: Optimize for battery life
5. **OTA Updates**: Over-the-air firmware updates via WiFi
6. **Device Management**: Cloud integration for monitoring and control

## 🐛 Common Issues for Beginners

### Compilation Errors
- **"use of moved value"**: Rust ownership - you can only use a value once unless you clone it
- **"cannot borrow as mutable"**: Need `mut` keyword for modifiable variables
- **"async function"**: Remember to use `.await` when calling async functions

### Runtime Issues
- **Serial output**: Use `info!()`, `warn!()`, `error!()` for debugging
- **Panic on unwrap()**: Use `match` or `if let` for better error handling
- **Task not running**: Make sure you `spawn()` the task and the main loop doesn't exit

## 📚 Learning Resources

- [Rust Book](https://doc.rust-lang.org/book/) - Learn Rust fundamentals
- [Embassy Book](https://embassy.dev/book/) - Async embedded programming
- [ESP-IDF Programming Guide](https://docs.espressif.com/projects/esp-idf/en/latest/) - ESP32 hardware
- [Bluetooth Low Energy Guide](https://learn.adafruit.com/introduction-to-bluetooth-low-energy) - BLE concepts

## 🤝 Contributing

This is an educational project! If you find ways to make the code clearer for beginners or have suggestions for better comments, please contribute.

---

**Happy coding! 🦀⚡** 