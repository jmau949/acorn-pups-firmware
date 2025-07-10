# ESP32 Rust Firmware Setup Guide

This guide will help you set up your development environment for building and flashing ESP32 firmware using Rust on Linux (Pop!_OS, Ubuntu, Debian).



## 1

```sh
sudo apt-get install -y gcc build-essential curl pkg-config

cargo install cargo-espflash espflash ldproxy

cargo install espup --locked


. /home/jodan/export-esp.sh
```

---



### c. Verify the toolchain

```sh
rustup show
```
- You should see a toolchain called `esp`.

---

## 5. ESP-IDF (C SDK) Setup (Optional)

Some crates (like `esp-idf-sys`) will automatically download and manage ESP-IDF. If you want to use the C SDK directly, clone and install it:

```sh
git clone --recursive https://github.com/espressif/esp-idf.git
cd esp-idf
./install.sh esp32
. ./export.sh
```

---

## 6. Build the Project

From the project root:

```sh
cargo build
```

---

## 7. Flashing and Serial Monitor

Install the flashing tools:

```sh
cargo install cargo-espflash espflash ldproxy
```

To flash your firmware and open a serial monitor:

```sh
cargo espflash /dev/ttyUSB0
# Replace /dev/ttyUSB0 with your ESP32's serial port
```

---

## 8. Troubleshooting

### Error: `custom toolchain 'esp' specified in override file ... is not installed`
- This means the ESP Rust toolchain is missing.
- Fix: Run `cargo install espup` and then `espup install`.

### Error: Python venv not found
- Run: `sudo apt install python3-venv`

### Other Issues
- Ensure you have all dependencies from step 2.
- Restart your terminal after installation steps.
- If you encounter build errors, try `cargo clean` and rebuild.

---

## 9. References
- [Rust on ESP Book](https://esp-rs.github.io/book/)
- [espup GitHub](https://github.com/esp-rs/espup)
- [esp-idf-sys crate](https://github.com/esp-rs/esp-idf-sys)

---

Happy hacking with Rust on ESP32! 