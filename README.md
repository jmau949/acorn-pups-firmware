# ESP32 LED Flashing Demo - Wokwi Simulation

This project demonstrates a multi-pattern LED flashing system on an ESP32 using the Wokwi simulator. It's designed to test that your ESP-IDF Rust environment and Wokwi simulator are working correctly.

## Hardware Setup (Simulated)

The Wokwi diagram includes:
- **ESP32 DevKit-C V4** board
- **3 LEDs**: Red (GPIO2), Green (GPIO4), Blue (GPIO5)
- **3 Resistors**: 220Ω current limiting resistors for each LED

## LED Patterns

The program cycles through 4 different LED patterns:

1. **Sequential Flashing**: Red → Green → Blue (repeats 3 times)
2. **All LEDs Together**: All three LEDs flash simultaneously (5 times)
3. **Alternating Pairs**: Red+Blue together, then Green alone (4 cycles)
4. **Fast Individual Blinks**: Each LED blinks rapidly in sequence (6 blinks each)

## How to Run

### Option 1: Using the PowerShell Script (Recommended)

```powershell
# Run the convenient PowerShell script
.\run_wokwi.ps1
```

### Option 2: Manual Commands

```powershell
# Build the project
cargo build

# Run in Wokwi (requires Wokwi CLI)
wokwi-cli diagram.json
```

## Prerequisites

1. **Rust ESP Environment**: Ensure you have the ESP-IDF Rust toolchain installed
2. **Wokwi CLI**: Install the Wokwi command-line interface:
   ```powershell
   npm install -g wokwi-cli
   ```

## What to Expect

When you run the simulation:

1. **Browser Opens**: Wokwi will open in your default browser
2. **Visual Feedback**: You'll see the ESP32 board with three LEDs that light up according to the patterns
3. **Serial Monitor**: Check the serial monitor output for pattern change notifications
4. **Continuous Loop**: The patterns repeat indefinitely until you stop the simulation

## Serial Output

The program logs information to help you understand what's happening:

```
I (XXX) pup: Starting LED Flashing Demo!
I (XXX) pup: LEDs initialized on GPIO2 (Red), GPIO4 (Green), GPIO5 (Blue)
I (XXX) pup: Pattern 1: Sequential flashing
I (XXX) pup: Pattern 2: All LEDs flashing together
I (XXX) pup: Pattern 3: Alternating pairs
I (XXX) pup: Pattern 4: Fast individual blinks
```

## Troubleshooting

### Build Issues
- Ensure ESP-IDF and Rust toolchain are properly installed
- Check that you're in the correct directory (`C:\esp\pup`)

### Wokwi Issues
- Install Wokwi CLI: `npm install -g wokwi-cli`
- Check that Node.js is installed for npm commands

### Simulation Not Starting
- Verify the build completed successfully (`cargo build`)
- Check that `diagram.json` exists in the project root
- Ensure your browser allows pop-ups from localhost

## Files Overview

- **`src/main.rs`**: Main program with LED control logic
- **`diagram.json`**: Wokwi hardware configuration (ESP32 + LEDs + resistors)
- **`wokwi.toml`**: Wokwi simulation settings
- **`run_wokwi.ps1`**: Convenience script to build and run
- **`Cargo.toml`**: Rust project configuration with ESP-IDF dependencies

## Customization

You can modify the LED patterns by editing `src/main.rs`:
- Change timing by adjusting `Duration::from_millis()` values
- Add new patterns in the match statement
- Use different GPIO pins (update both code and `diagram.json`)

Enjoy your ESP32 LED flashing demo! 🎉 