use uuid::Uuid;

/// Generate new device instance ID (proper UUID v4) for registration security
pub fn generate_device_instance_id() -> String {
    // Generate proper RFC-compliant UUID v4 (random)
    // This ensures compatibility with backend validation and uniqueness
    Uuid::new_v4().to_string()
}

/// Get the device serial number based on MAC address
pub fn get_device_serial_number() -> String {
    // Get the default MAC address which is unique for each ESP32
    let mut mac = [0u8; 6];
    unsafe {
        esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr());
    }
    // Create a serial number from the MAC address
    format!(
        "ESP32-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Get the device MAC address in standard format
pub fn get_device_mac_address() -> String {
    let mut mac = [0u8; 6];
    unsafe {
        esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr());
    }
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}