use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::{debug, info};

/// Comprehensive NVS flash storage dump function
/// This dumps all namespaces and their key-value pairs for debugging
pub fn dump_entire_nvs_storage(nvs_partition: &EspDefaultNvsPartition) {
    info!("🔍 Starting comprehensive NVS flash storage analysis...");
    info!("📱 NVS Partition: Initialized and ready");

    // List of known namespaces to check
    let known_namespaces = [
        "nvs.net80211",  // WiFi system data
        "wifi_config",   // Our WiFi credentials
        "reset_state",   // Device instance ID and reset state (Echo/Nest-style)
        "mqtt_certs",    // MQTT certificates
        "acorn_device",  // Device-specific data
        "nvs",           // Default namespace
        "phy_init",      // PHY calibration data
        "tcpip_adapter", // TCP/IP adapter config
    ];

    info!("🔍 Checking {} known namespaces...", known_namespaces.len());

    for namespace in &known_namespaces {
        info!("🔍 === Checking namespace: '{}' ===", namespace);

        match EspNvs::new(nvs_partition.clone(), namespace, true) {
            Ok(nvs_handle) => {
                info!("✅ Opened namespace '{}' successfully", namespace);
                dump_namespace_contents(&nvs_handle, namespace);
            }
            Err(e) => {
                info!("📭 Namespace '{}' not found or empty: {:?}", namespace, e);
            }
        }
    }

    info!("🔍 NVS flash storage dump completed");
}

/// Dump known keys from a specific NVS namespace
fn dump_namespace_contents(nvs_handle: &EspNvs<NvsDefault>, namespace: &str) {
    info!(
        "📂 Attempting to dump known keys from namespace: '{}'",
        namespace
    );

    // Known keys for different namespaces
    let known_keys = match namespace {
        "wifi_config" => vec![
            "ssid",
            "password",
            "auth_token",
            "device_name",
            "user_timezone",
            "timestamp",
        ],
        "reset_state" => vec![
            "device_instance_id",
            "device_state",
            "reset_timestamp",
            "reset_reason",
        ],
        "mqtt_certs" => vec![
            "device_cert",
            "private_key",
            "ca_cert",
            "iot_endpoint",
            "device_id",
        ],
        "acorn_device" => vec!["device_id", "serial_number", "firmware_version"],
        _ => vec![""], // Try empty key for other namespaces
    };

    let mut found_keys = 0;

    for key in &known_keys {
        if key.is_empty() && namespace != "nvs" {
            continue;
        }

        // Try reading as string first
        match nvs_handle.get_str(key, &mut [0u8; 512]) {
            Ok(Some(value)) => {
                info!("   📝 '{}' = '{}' (string)", key, value);
                found_keys += 1;
                continue;
            }
            Ok(None) => {
                // Key doesn't exist as string, try other types
            }
            Err(_) => {
                // Not a string, try other types
            }
        }

        // Try reading as blob
        let mut buffer = vec![0u8; 1024];
        match nvs_handle.get_blob(key, &mut buffer) {
            Ok(Some(blob_data)) => {
                let size = blob_data.len();
                info!("   💾 '{}' = <blob {} bytes>", key, size);

                // Display first 32 bytes as hex if data exists
                if size > 0 {
                    let display_bytes = std::cmp::min(size, 32);
                    let hex_string: String = blob_data[..display_bytes]
                        .iter()
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(" ");

                    info!(
                        "       Hex: {} {}",
                        hex_string,
                        if size > 32 { "..." } else { "" }
                    );

                    // Try to display as UTF-8 if possible
                    if let Ok(utf8_str) = std::str::from_utf8(blob_data) {
                        let display_str = if utf8_str.len() > 64 {
                            format!("{}...", &utf8_str[..64])
                        } else {
                            utf8_str.to_string()
                        };
                        info!("       UTF8: '{}'", display_str);
                    }
                }
                found_keys += 1;
                continue;
            }
            Ok(None) => {
                // Not a blob
            }
            Err(_) => {
                // Error reading as blob
            }
        }

        // Try reading as u32
        match nvs_handle.get_u32(key) {
            Ok(Some(value)) => {
                info!("   🔢 '{}' = {} (u32)", key, value);
                found_keys += 1;
                continue;
            }
            Ok(None) => {
                // Not a u32
            }
            Err(_) => {
                // Error reading as u32
            }
        }

        // Try reading as u64
        match nvs_handle.get_u64(key) {
            Ok(Some(value)) => {
                info!("   🔢 '{}' = {} (u64)", key, value);
                found_keys += 1;
                continue;
            }
            Ok(None) => {
                // Not a u64
            }
            Err(_) => {
                // Error reading as u64
            }
        }

        // Key not found in any format
        debug!("   ❓ '{}' = <not found>", key);
    }

    if found_keys == 0 {
        info!("📭 No known keys found in namespace '{}'", namespace);
    } else {
        info!(
            "📊 Found {} known keys in namespace '{}'",
            found_keys, namespace
        );
    }
}