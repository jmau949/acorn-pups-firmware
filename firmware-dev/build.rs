// Build script for ESP32 development firmware
// This fixes common ESP32 toolchain issues

use std::env;

fn main() {
    // Set ESP_IDF_SYS_ROOT_CRATE to help esp-idf-sys find the root crate
    if env::var("ESP_IDF_SYS_ROOT_CRATE").is_err() {
        println!("cargo:rustc-env=ESP_IDF_SYS_ROOT_CRATE=pup-dev");
    }

    // Set other helpful environment variables
    if env::var("ESP_IDF_VERSION").is_err() {
        println!("cargo:rustc-env=ESP_IDF_VERSION=v5.1.2");
    }

    // Tell cargo to rebuild if any environment variable changes
    println!("cargo:rerun-if-env-changed=ESP_IDF_SYS_ROOT_CRATE");
    println!("cargo:rerun-if-env-changed=ESP_IDF_VERSION");

    // Call the main embuild function
    embuild::espidf::sysenv::output();
}
