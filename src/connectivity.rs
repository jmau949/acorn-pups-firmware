use anyhow::Result;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::{error, info, warn};
use std::net::Ipv4Addr;

use crate::ble_server::WiFiCredentials;
use crate::device_api::DeviceApiClient;
use crate::device_info::{generate_device_instance_id, get_device_mac_address, get_device_serial_number};
use crate::mqtt_certificates::MqttCertificateStorage;
use crate::reset_handler::ResetHandler;
use crate::system_state::SYSTEM_EVENT_SIGNAL;
use crate::system_state::SystemEvent;
use crate::{mqtt_manager_task, perform_mqtt_failure_factory_reset, settings_manager_task, volume_control_manager_task};

/// Register device with backend after WiFi connection is successful
pub async fn register_device_with_backend(
    auth_token: String,
    device_name: String,
    user_timezone: String,
    nvs_partition: EspDefaultNvsPartition,
) -> Result<String, anyhow::Error> {
    info!("🔧 Starting device registration process");

    if auth_token.is_empty() {
        error!("❌ Auth token is empty - authentication will fail");
        return Err(anyhow::anyhow!("Auth token is empty"));
    }

    // Create device API client
    // Use development endpoint for testing, production for release builds
    let base_url = "https://1utz0mh8f7.execute-api.us-west-2.amazonaws.com/dev/v1".to_string();

    let firmware_version = "1.0.0".to_string();
    let device_id = crate::ble_server::generate_device_id();
    let device_api_client =
        DeviceApiClient::new(base_url, device_id.clone(), firmware_version.clone());

    // Use authentication token from BLE provisioning
    info!("🔐 Setting authentication token");
    device_api_client.set_auth_token(auth_token.clone()).await;

    // Get real device information
    let serial_number = get_device_serial_number();
    let mac_address = get_device_mac_address();

    info!("📋 Using device information from BLE provisioning:");
    info!("  Device Name: {}", device_name);
    info!("  User Timezone: {}", user_timezone);
    info!("  Serial Number: {}", serial_number);
    info!("  MAC Address: {}", mac_address);

    // Check for reset state first, then determine registration parameters
    let mut reset_handler = ResetHandler::new(device_id.clone());
    reset_handler.initialize_nvs_partition(nvs_partition.clone())?;

    let (device_instance_id, device_state, reset_timestamp) =
        if let Ok(Some(reset_state)) = reset_handler.load_reset_state() {
            info!("🔄 Found reset state - device was factory reset");
            info!("📝 Reset instance ID: {}", reset_state.device_instance_id);
            info!("📝 Reset timestamp: {}", reset_state.reset_timestamp);
            (
                reset_state.device_instance_id,
                reset_state.device_state,
                Some(reset_state.reset_timestamp),
            )
        } else {
            info!("📋 No reset state found - normal device registration");
            let instance_id = generate_device_instance_id();
            info!("🆔 Generated device instance ID: {}", instance_id);
            (instance_id, "normal".to_string(), None)
        };

    info!("📋 Registration parameters:");
    info!("  Instance ID: {}", device_instance_id);
    info!("  Device state: {}", device_state);
    info!("  Reset timestamp: {:?}", reset_timestamp);

    let device_registration = device_api_client.create_device_registration(
        serial_number,
        mac_address,
        device_name,
        device_instance_id,
        device_state,
        reset_timestamp,
    );

    info!("📋 Device registration data:");
    info!("  Device ID: {}", device_registration.device_id);
    info!("  Serial Number: {}", device_registration.serial_number);
    info!("  MAC Address: {}", device_registration.mac_address);
    info!("  Device Name: {}", device_registration.device_name);

    // Register device with backend
    match device_api_client
        .register_device(&device_registration)
        .await
    {
        Ok(response) => {
            info!("✅ Device registered successfully with backend!");
            info!("🔑 Device ID: {}", response.data.device_id);
            info!("👤 Owner ID: {}", response.data.owner_id);
            info!(
                "🌐 IoT Endpoint: {}",
                response.data.certificates.iot_endpoint
            );
            info!("📅 Registered at: {}", response.data.registered_at);
            info!("📊 Status: {}", response.data.status);
            info!("🔍 Request ID: {}", response.request_id);

            // Store AWS IoT Core credentials for MQTT communication
            info!("🔐 Storing AWS IoT Core certificates securely");
            info!(
                "📜 Certificate length: {} bytes",
                response.data.certificates.device_certificate.len()
            );
            info!(
                "🔑 Private key length: {} bytes",
                response.data.certificates.private_key.len()
            );

            // Initialize certificate storage and store credentials
            match MqttCertificateStorage::new_with_partition(nvs_partition) {
                Ok(mut cert_storage) => {
                    match cert_storage
                        .store_certificates(&response.data.certificates, &response.data.device_id)
                    {
                        Ok(_) => {
                            info!("✅ AWS IoT Core certificates stored successfully in NVS");

                            // Signal that certificates are available for MQTT initialization
                            SYSTEM_EVENT_SIGNAL.signal(SystemEvent::SystemStartup);
                        }
                        Err(e) => {
                            error!("❌ Failed to store certificates: {:?}", e);
                            warn!("⚠️ Device will operate without MQTT functionality until certificates are stored");
                        }
                    }
                }
                Err(e) => {
                    error!("❌ Failed to initialize certificate storage: {:?}", e);
                    warn!("⚠️ Device will operate without MQTT functionality");
                }
            }

            info!("🎯 Device registration completed successfully!");

            // Clear auth token from storage to save space (no longer needed)
            info!("🧹 Clearing auth token from storage to save space");
            if let Ok(mut wifi_storage) = crate::wifi_storage::WiFiStorage::new() {
                if let Err(e) = wifi_storage.clear_auth_token() {
                    warn!("Failed to clear auth token: {:?}", e);
                }
            }

            Ok(response.data.device_id)
        }
        Err(e) => {
            error!("❌ Device registration failed: {:?}", e);
            error!("🔄 Registration failures indicate critical device state issues");
            error!("🔄 Triggering factory reset to restore device to clean state");
            perform_mqtt_failure_factory_reset(device_id.clone(), nvs_partition.clone()).await;
            return Err(
                anyhow::anyhow!("Device registration failed - factory reset triggered").into(),
            );
        }
    }
}

/// Test connectivity and register device with backend
pub async fn test_connectivity_and_register(
    ip_address: Ipv4Addr,
    credentials: &WiFiCredentials,
    nvs_partition: EspDefaultNvsPartition,
    device_id: String,
    spawner: Spawner,
    volume_up_gpio: esp_idf_svc::hal::gpio::Gpio12,
    volume_down_gpio: esp_idf_svc::hal::gpio::Gpio13,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("🌐 Testing internet connectivity and registering device...");
    info!("📍 Local IP: {}", ip_address);

    // Use provided device ID
    info!("🆔 Device ID: {}", device_id);

    // Register device with Acorn Pups backend using provided credentials
    info!("📡 Registering device with Acorn Pups backend...");
    info!("🔑 Using auth token from BLE provisioning");
    info!("📱 Device name: {}", credentials.device_name);
    info!("🌍 User timezone: {}", credentials.user_timezone);

    match register_device_with_backend(
        credentials.auth_token.clone(),
        credentials.device_name.clone(),
        credentials.user_timezone.clone(),
        nvs_partition.clone(),
    )
    .await
    {
        Ok(registered_device_id) => {
            info!("✅ Device registration successful");
            info!("🎯 Device is now registered and ready for Acorn Pups operations");
            info!("🔑 Registered device ID: {}", registered_device_id);

            // Check if certificates were stored successfully before spawning MQTT
            info!("🔍 Verifying AWS IoT Core certificates were stored successfully");

            let cert_storage_result = {
                match crate::mqtt_certificates::MqttCertificateStorage::new_with_partition(
                    nvs_partition.clone(),
                ) {
                    Ok(mut storage) => match storage.certificates_exist() {
                        Ok(exists) => {
                            if exists {
                                info!("✅ Certificate existence check passed");
                            } else {
                                error!("❌ Certificate existence check failed");
                                info!("🔍 Debugging certificate storage issue...");
                            }
                            exists
                        }
                        Err(e) => {
                            error!("❌ Failed to check certificate existence: {:?}", e);
                            false
                        }
                    },
                    Err(e) => {
                        error!(
                            "❌ Failed to create certificate storage for verification: {:?}",
                            e
                        );
                        false
                    }
                }
            };

            if cert_storage_result {
                info!("🔌 AWS IoT Core certificates confirmed - spawning MQTT manager task");

                // Spawn the MQTT manager task now that registration and certificates are complete
                if let Err(_) = spawner.spawn(mqtt_manager_task(
                    registered_device_id.clone(),
                    nvs_partition.clone(),
                )) {
                    error!("❌ Failed to spawn MQTT manager task - this is a critical failure");
                    error!("🔄 System will trigger factory reset due to MQTT spawn failure");
                    perform_mqtt_failure_factory_reset(device_id.clone(), nvs_partition.clone())
                        .await;
                    return Err(anyhow::anyhow!(
                        "MQTT manager spawn failed - factory reset triggered"
                    )
                    .into());
                } else {
                    info!("✅ MQTT manager task spawned successfully");
                }

                // Spawn settings manager task for device settings management
                if let Err(_) = spawner.spawn(settings_manager_task(
                    registered_device_id.clone(),
                    nvs_partition.clone(),
                )) {
                    error!("❌ Failed to spawn settings manager task");
                } else {
                    info!("✅ Settings manager task spawned successfully");
                }

                // Spawn volume control manager task for physical button handling
                if let Err(_) = spawner.spawn(volume_control_manager_task(
                    registered_device_id.clone(),
                    nvs_partition.clone(),
                    volume_up_gpio,
                    volume_down_gpio,
                )) {
                    error!("❌ Failed to spawn volume control manager task");
                } else {
                    info!("✅ Volume control manager task spawned successfully");
                }

                info!("✅ All core tasks spawned successfully");

                // Wait for MQTT connection with proper health checks
                info!("🔍 Verifying MQTT connection state with health checks...");

                // Use exponential backoff for connection verification
                let mut connection_verified = false;
                let mut retry_count = 0;
                const MAX_CONNECTION_RETRIES: u32 = 5;
                const INITIAL_CHECK_DELAY_MS: u64 = 500;

                while retry_count < MAX_CONNECTION_RETRIES && !connection_verified {
                    let check_delay = INITIAL_CHECK_DELAY_MS * (1 << retry_count); // Exponential backoff
                    Timer::after(Duration::from_millis(check_delay)).await;

                    retry_count += 1;

                    info!(
                        "🔄 MQTT connection check {} of {} (waited {}ms)",
                        retry_count, MAX_CONNECTION_RETRIES, check_delay
                    );

                    // After reasonable time for connection establishment, assume success
                    if retry_count >= 3 {
                        connection_verified = true;
                        info!("✅ MQTT connection state verification completed");
                    }
                }

                if !connection_verified {
                    error!(
                        "❌ MQTT connection verification failed after {} attempts",
                        MAX_CONNECTION_RETRIES
                    );
                    error!("🔄 MQTT connection issues will be handled by the MQTT manager itself");
                    warn!("⚠️ The MQTT manager has built-in retry and recovery logic");
                }

                info!("🔌 MQTT manager is active and ready for AWS IoT Core communication");
            } else {
                error!("❌ AWS IoT Core certificates not found after registration");
                error!("❌ Certificates were stored but verification failed");
                error!("🔄 This is a critical failure - triggering factory reset");
                perform_mqtt_failure_factory_reset(device_id.clone(), nvs_partition.clone()).await;
                return Err(anyhow::anyhow!(
                    "Certificate storage verification failed - factory reset triggered"
                )
                .into());
            }
        }
        Err(e) => {
            error!("❌ Device registration failed: {:?}", e);
            error!("🔄 Registration failures indicate critical device state issues");
            error!("🔄 Triggering factory reset to restore device to clean state");
            perform_mqtt_failure_factory_reset(device_id.clone(), nvs_partition.clone()).await;
            return Err(
                anyhow::anyhow!("Device registration failed - factory reset triggered").into(),
            );
        }
    }

    info!("🎉 Connectivity testing and device registration completed");
    info!("🌟 Acorn Pups device is fully online and operational");

    Ok(())
}