# ESP32 Rust Firmware Setup Guide

This guide will help you set up your development environment for building and flashing ESP32 firmware using Rust on Linux (Pop!_OS, Ubuntu, Debian).



## 1

```sh
sudo apt-get install -y gcc build-essential curl pkg-config

cargo install espflash ldproxy

cargo install espup --locked


. /home/jodan/export-esp.sh


sudo usermod -a -G dialout jodan

newgrp dialout

groups
# Should show: dialout jodan adm sudo lpadmin

cargo run


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

