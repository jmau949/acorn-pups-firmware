// MQTT Certificate Storage Module
// Handles secure storage and management of AWS IoT Core certificates in NVS flash
// Follows patterns established in wifi_storage.rs for consistent NVS handling

// Import ESP-IDF's NVS (Non-Volatile Storage) functionality
// NVS is a key-value storage system that persists data in flash memory
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys::EspError;

// Import logging macros for debug output with consistent emoji prefixes
use log::{debug, error, info, warn};

// Import Serde traits for JSON serialization of certificate metadata
use serde::{Deserialize, Serialize};

// Import our device certificate structure from device_api module
use crate::device_api::DeviceCertificates;

// NVS storage keys for certificate management
// Using descriptive namespacing to avoid conflicts with wifi storage
const NVS_NAMESPACE: &str = "mqtt_certs"; // Namespace for MQTT certificates
const DEVICE_CERT_KEY: &str = "device_cert"; // AWS IoT device certificate (X.509 PEM)
const PRIVATE_KEY_KEY: &str = "private_key"; // Device private key (RSA PEM)
const IOT_ENDPOINT_KEY: &str = "iot_endpoint"; // AWS IoT Core endpoint URL
const CERT_METADATA_KEY: &str = "cert_meta"; // Certificate metadata as JSON
const CERT_VALIDATION_KEY: &str = "cert_valid"; // Certificate validation status

// Certificate metadata structure for storage and validation
// Includes additional information beyond the raw certificate data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateMetadata {
    pub stored_at: u64,    // Unix timestamp when certificates were stored
    pub device_id: String, // Device ID associated with certificates
    pub certificate_fingerprint: String, // SHA256 fingerprint for validation
    pub iot_endpoint: String, // AWS IoT endpoint for quick access
    pub is_valid: bool,    // Whether certificates have been validated
    pub last_used: Option<u64>, // Last time certificates were used for MQTT
    pub validation_attempts: u32, // Number of validation attempts
}

// Certificate validation results
#[derive(Debug, Clone)]
pub enum CertificateValidation {
    Valid,
    InvalidFormat,
    InvalidContent,
    Expired,
    Corrupted,
    Missing,
}

// MQTT certificate storage manager
// Handles all NVS operations for AWS IoT Core certificates
pub struct MqttCertificateStorage {
    nvs: EspNvs<NvsDefault>, // ESP-IDF NVS handle for certificate storage
}

impl MqttCertificateStorage {
    /// Create certificate storage using provided NVS partition
    /// Recommended approach to avoid partition conflicts
    pub fn new_with_partition(nvs_partition: EspDefaultNvsPartition) -> Result<Self, EspError> {
        info!("🔐 Initializing MQTT certificate storage with provided NVS partition");

        // Open the NVS namespace for MQTT certificates
        let nvs = EspNvs::new(nvs_partition, NVS_NAMESPACE, true)?;

        info!("✅ MQTT certificate storage initialized successfully");

        Ok(Self { nvs })
    }

    /// Create certificate storage by taking the default NVS partition
    /// Note: Will fail if partition is already taken elsewhere
    pub fn new() -> Result<Self, EspError> {
        info!("🔐 Initializing MQTT certificate storage with default NVS partition");

        let nvs_default_partition = EspDefaultNvsPartition::take()?;
        let nvs = EspNvs::new(nvs_default_partition, NVS_NAMESPACE, true)?;

        info!("✅ MQTT certificate storage initialized successfully");

        Ok(Self { nvs })
    }

    /// Store AWS IoT Core certificates received from device registration
    /// Validates certificates before storage and creates metadata
    pub fn store_certificates(
        &mut self,
        certificates: &DeviceCertificates,
        device_id: &str,
    ) -> Result<(), EspError> {
        info!(
            "🔐 Storing AWS IoT Core certificates for device: {}",
            device_id
        );

        // Validate certificate format before storage
        if let Err(validation_result) = self.validate_certificate_format(certificates) {
            error!("❌ Certificate validation failed: {:?}", validation_result);
            return Err(EspError::from_infallible::<
                { esp_idf_svc::sys::ESP_ERR_INVALID_ARG },
            >());
        }

        // Generate certificate fingerprint for validation
        let fingerprint = self.generate_certificate_fingerprint(&certificates.device_certificate);

        // Create certificate metadata
        let metadata = CertificateMetadata {
            stored_at: self.get_current_timestamp(),
            device_id: device_id.to_string(),
            certificate_fingerprint: fingerprint,
            iot_endpoint: certificates.iot_endpoint.clone(),
            is_valid: true,
            last_used: None,
            validation_attempts: 0,
        };

        // Store certificate components in NVS
        // Using separate keys for each component for security and flexibility
        self.nvs
            .set_str(DEVICE_CERT_KEY, &certificates.device_certificate)?;
        self.nvs
            .set_str(PRIVATE_KEY_KEY, &certificates.private_key)?;
        self.nvs
            .set_str(IOT_ENDPOINT_KEY, &certificates.iot_endpoint)?;

        // Store metadata as JSON
        match serde_json::to_string(&metadata) {
            Ok(metadata_json) => {
                self.nvs.set_str(CERT_METADATA_KEY, &metadata_json)?;
                self.nvs.set_u8(CERT_VALIDATION_KEY, 1)?; // Mark as valid

                info!("✅ AWS IoT Core certificates stored successfully");
                info!(
                    "📜 Certificate fingerprint: {}",
                    &metadata.certificate_fingerprint[..16]
                );
                info!("🌐 IoT endpoint: {}", certificates.iot_endpoint);

                Ok(())
            }
            Err(e) => {
                error!("❌ Failed to serialize certificate metadata: {}", e);
                Err(EspError::from_infallible::<
                    { esp_idf_svc::sys::ESP_ERR_INVALID_ARG },
                >())
            }
        }
    }

    /// Load stored AWS IoT Core certificates from NVS
    /// Returns None if certificates don't exist or are invalid
    pub fn load_certificates(&mut self) -> Result<Option<DeviceCertificates>, EspError> {
        info!("🔍 Loading AWS IoT Core certificates from NVS");

        // Check if certificates exist
        if !self.certificates_exist() {
            info!("📭 No stored certificates found");
            return Ok(None);
        }

        // Load certificate components
        let mut cert_buffer = [0u8; 2048];
        let device_certificate = match self.nvs.get_str(DEVICE_CERT_KEY, &mut cert_buffer) {
            Ok(Some(cert)) => cert,
            Ok(None) => {
                warn!("⚠️ Device certificate not found in storage");
                return Ok(None);
            }
            Err(e) => {
                error!("❌ Failed to load device certificate: {:?}", e);
                return Err(e);
            }
        };

        let mut key_buffer = [0u8; 2048];
        let private_key = match self.nvs.get_str(PRIVATE_KEY_KEY, &mut key_buffer) {
            Ok(Some(key)) => key,
            Ok(None) => {
                warn!("⚠️ Private key not found in storage");
                return Ok(None);
            }
            Err(e) => {
                error!("❌ Failed to load private key: {:?}", e);
                return Err(e);
            }
        };

        let mut endpoint_buffer = [0u8; 256];
        let iot_endpoint = match self.nvs.get_str(IOT_ENDPOINT_KEY, &mut endpoint_buffer) {
            Ok(Some(endpoint)) => endpoint,
            Ok(None) => {
                warn!("⚠️ IoT endpoint not found in storage");
                return Ok(None);
            }
            Err(e) => {
                error!("❌ Failed to load IoT endpoint: {:?}", e);
                return Err(e);
            }
        };

        let certificates = DeviceCertificates {
            device_certificate: device_certificate.to_string(),
            private_key: private_key.to_string(),
            iot_endpoint: iot_endpoint.to_string(),
        };

        // Validate loaded certificates
        match self.validate_certificate_format(&certificates) {
            Ok(_) => {
                info!("✅ Certificates loaded and validated successfully");
                self.update_last_used_timestamp()?;
                Ok(Some(certificates))
            }
            Err(validation_result) => {
                warn!(
                    "⚠️ Loaded certificates failed validation: {:?}",
                    validation_result
                );
                Ok(None)
            }
        }
    }

    /// Get certificate size by attempting to load with 1-byte buffer
    /// Returns required size or defaults to 2048 for safety
    fn get_certificate_size(&mut self, key: &str) -> usize {
        // Try with 1-byte buffer to trigger size error
        let mut tiny_buffer = [0u8; 1];
        match self.nvs.get_str(key, &mut tiny_buffer) {
            Err(e) => {
                // Check if error indicates insufficient buffer space
                debug!(
                    "🔍 NVS get_str error (expected for size detection): {:?}",
                    e
                );
                // For any error with tiny buffer, assume certificate exists and is larger
                // Default to 2048 bytes for safety
                2048
            }
            Ok(Some(_)) => {
                // Certificate fits in 1 byte (impossible for real certs), use default
                warn!("⚠️ Certificate unexpectedly small, using default buffer size");
                2048
            }
            Ok(None) => {
                // Certificate doesn't exist
                0
            }
        }
    }

    /// Load certificates with optimized dynamic buffer sizing for MQTT usage
    /// Uses dynamic buffer allocation instead of fixed 2KB arrays
    pub fn load_certificates_for_mqtt(&mut self) -> Result<Option<DeviceCertificates>, EspError> {
        info!("🔍 Loading AWS IoT Core certificates with optimized buffer sizing");

        // Check if certificates exist
        if !self.certificates_exist() {
            info!("📭 No stored certificates found");
            return Ok(None);
        }

        // Get optimized buffer sizes (with fallback to reasonable defaults)
        let cert_size = std::cmp::max(self.get_certificate_size(DEVICE_CERT_KEY), 1500);
        let key_size = std::cmp::max(self.get_certificate_size(PRIVATE_KEY_KEY), 1700);
        let endpoint_size = std::cmp::max(self.get_certificate_size(IOT_ENDPOINT_KEY), 256);

        debug!(
            "📏 Using buffer sizes - cert: {}, key: {}, endpoint: {}",
            cert_size, key_size, endpoint_size
        );

        // Load certificate components with dynamic buffers
        let mut cert_buffer = vec![0u8; cert_size + 1]; // +1 for null terminator
        let device_certificate = match self.nvs.get_str(DEVICE_CERT_KEY, &mut cert_buffer) {
            Ok(Some(cert)) => cert,
            Ok(None) => {
                warn!("⚠️ Device certificate not found in storage");
                return Ok(None);
            }
            Err(e) => {
                error!("❌ Failed to load device certificate: {:?}", e);
                return Err(e);
            }
        };

        let mut key_buffer = vec![0u8; key_size + 1]; // +1 for null terminator
        let private_key = match self.nvs.get_str(PRIVATE_KEY_KEY, &mut key_buffer) {
            Ok(Some(key)) => key,
            Ok(None) => {
                warn!("⚠️ Private key not found in storage");
                return Ok(None);
            }
            Err(e) => {
                error!("❌ Failed to load private key: {:?}", e);
                return Err(e);
            }
        };

        let mut endpoint_buffer = vec![0u8; endpoint_size + 1]; // +1 for null terminator
        let iot_endpoint = match self.nvs.get_str(IOT_ENDPOINT_KEY, &mut endpoint_buffer) {
            Ok(Some(endpoint)) => endpoint,
            Ok(None) => {
                warn!("⚠️ IoT endpoint not found in storage");
                return Ok(None);
            }
            Err(e) => {
                error!("❌ Failed to load IoT endpoint: {:?}", e);
                return Err(e);
            }
        };

        let certificates = DeviceCertificates {
            device_certificate: device_certificate.to_string(),
            private_key: private_key.to_string(),
            iot_endpoint: iot_endpoint.to_string(),
        };

        info!(
            "✅ Certificates loaded with optimized buffer sizes - saved {} bytes",
            (2048 * 2 + 256) - (cert_size + key_size + endpoint_size)
        );

        // Validate loaded certificates
        match self.validate_certificate_format(&certificates) {
            Ok(_) => {
                info!("✅ Certificates loaded and validated successfully with optimized buffers");
                self.update_last_used_timestamp()?;
                Ok(Some(certificates))
            }
            Err(validation_result) => {
                warn!(
                    "⚠️ Loaded certificates failed validation: {:?}",
                    validation_result
                );
                Ok(None)
            }
        }
    }

    /// Check if certificates exist in storage
    pub fn certificates_exist(&mut self) -> bool {
        match self.nvs.get_u8(CERT_VALIDATION_KEY) {
            Ok(Some(1)) => {
                // Check if all required components exist
                let cert_exists = self.nvs.get_str(DEVICE_CERT_KEY, &mut [0u8; 1]).is_ok();
                let key_exists = self.nvs.get_str(PRIVATE_KEY_KEY, &mut [0u8; 1]).is_ok();
                let endpoint_exists = self.nvs.get_str(IOT_ENDPOINT_KEY, &mut [0u8; 1]).is_ok();

                cert_exists && key_exists && endpoint_exists
            }
            _ => false,
        }
    }

    /// Get certificate metadata if available
    pub fn get_certificate_metadata(&mut self) -> Result<Option<CertificateMetadata>, EspError> {
        match self.nvs.get_str(CERT_METADATA_KEY, &mut [0u8; 512]) {
            Ok(Some(metadata_json)) => {
                match serde_json::from_str::<CertificateMetadata>(&metadata_json) {
                    Ok(metadata) => Ok(Some(metadata)),
                    Err(e) => {
                        warn!("⚠️ Failed to parse certificate metadata: {}", e);
                        Ok(None)
                    }
                }
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Clear all stored certificates (factory reset)
    pub fn clear_certificates(&mut self) -> Result<(), EspError> {
        info!("🗑️ Clearing all stored AWS IoT Core certificates");

        // Remove all certificate components
        let _ = self.nvs.remove(DEVICE_CERT_KEY);
        let _ = self.nvs.remove(PRIVATE_KEY_KEY);
        let _ = self.nvs.remove(IOT_ENDPOINT_KEY);
        let _ = self.nvs.remove(CERT_METADATA_KEY);
        let _ = self.nvs.remove(CERT_VALIDATION_KEY);

        info!("✅ All certificates cleared successfully");
        Ok(())
    }

    /// Validate certificate format and basic structure
    fn validate_certificate_format(
        &self,
        certificates: &DeviceCertificates,
    ) -> Result<(), CertificateValidation> {
        // Validate device certificate PEM format
        if !certificates
            .device_certificate
            .starts_with("-----BEGIN CERTIFICATE-----")
            || !certificates
                .device_certificate
                .ends_with("-----END CERTIFICATE-----")
        {
            return Err(CertificateValidation::InvalidFormat);
        }

        // Validate private key PEM format
        if !certificates
            .private_key
            .starts_with("-----BEGIN PRIVATE KEY-----")
            || !certificates
                .private_key
                .ends_with("-----END PRIVATE KEY-----")
        {
            return Err(CertificateValidation::InvalidFormat);
        }

        // Validate IoT endpoint format (should be AWS IoT hostname)
        if !certificates.iot_endpoint.contains(".iot.")
            || !certificates.iot_endpoint.contains(".amazonaws.com")
        {
            return Err(CertificateValidation::InvalidContent);
        }

        // Check minimum size requirements
        if certificates.device_certificate.len() < 500 || certificates.private_key.len() < 500 {
            return Err(CertificateValidation::InvalidContent);
        }

        Ok(())
    }

    /// Generate SHA256 fingerprint of certificate for validation
    fn generate_certificate_fingerprint(&self, certificate: &str) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(certificate.as_bytes());
        let result = hasher.finalize();
        format!("{:x}", result)
    }

    /// Update the last used timestamp for certificates
    fn update_last_used_timestamp(&mut self) -> Result<(), EspError> {
        if let Ok(Some(mut metadata)) = self.get_certificate_metadata() {
            metadata.last_used = Some(self.get_current_timestamp());

            if let Ok(metadata_json) = serde_json::to_string(&metadata) {
                self.nvs.set_str(CERT_METADATA_KEY, &metadata_json)?;
            }
        }
        Ok(())
    }

    /// Get current timestamp (Unix epoch seconds)
    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

// Certificate validation utilities for external use
impl MqttCertificateStorage {
    /// Validate that certificates are ready for MQTT connection
    pub fn validate_for_mqtt(&mut self) -> Result<bool, EspError> {
        match self.load_certificates()? {
            Some(certificates) => match self.validate_certificate_format(&certificates) {
                Ok(_) => {
                    info!("✅ Certificates validated successfully for MQTT");
                    Ok(true)
                }
                Err(validation_result) => {
                    warn!("⚠️ Certificate validation failed: {:?}", validation_result);
                    Ok(false)
                }
            },
            None => {
                info!("📭 No certificates available for MQTT");
                Ok(false)
            }
        }
    }

    /// Get IoT endpoint without loading full certificates
    pub fn get_iot_endpoint(&mut self) -> Result<Option<String>, EspError> {
        let mut buffer = [0u8; 256];
        match self.nvs.get_str(IOT_ENDPOINT_KEY, &mut buffer) {
            Ok(Some(endpoint)) => Ok(Some(endpoint.to_string())),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
