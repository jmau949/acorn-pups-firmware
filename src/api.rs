use anyhow::{anyhow, Result};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_svc::http::Method;
use embedded_svc::io::{Read, Write};
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Response data structure for API calls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseData {
    pub status_code: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Configuration for the API client
#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub max_redirects: u32,
    pub buffer_size: usize,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.example.com".to_string(),
            timeout: Duration::from_secs(30),
            max_redirects: 5,
            buffer_size: 4096,
        }
    }
}

/// Thread-safe HTTP API client for ESP32 with singleton pattern
#[derive(Clone)]
pub struct ApiClient {
    inner: Arc<Mutex<CriticalSectionRawMutex, ApiClientInner>>,
}

struct ApiClientInner {
    base_url: String,
    token: Option<String>,
    config: ApiConfig,
}

impl ApiClient {
    /// Create a new API client with default configuration
    pub fn new(base_url: String) -> Self {
        let config = ApiConfig {
            base_url: base_url.clone(),
            ..Default::default()
        };

        Self {
            inner: Arc::new(Mutex::new(ApiClientInner {
                base_url,
                token: None,
                config,
            })),
        }
    }

    /// Create a new API client with custom configuration
    pub fn with_config(config: ApiConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ApiClientInner {
                base_url: config.base_url.clone(),
                token: None,
                config,
            })),
        }
    }

    /// Set the authorization token for all requests
    pub async fn with_token(self, token: String) -> Self {
        let mut inner = self.inner.lock().await;
        inner.token = Some(token);
        drop(inner);
        self
    }

    /// Set the authorization token for an existing client
    pub async fn set_token(&self, token: String) {
        let mut inner = self.inner.lock().await;
        inner.token = Some(token);
    }

    /// Clear the authorization token
    pub async fn clear_token(&self) {
        let mut inner = self.inner.lock().await;
        inner.token = None;
    }

    /// Get or create the HTTP connection (creates new connection each time)
    async fn get_connection(&self) -> Result<EspHttpConnection> {
        let inner = self.inner.lock().await;

        debug!("Creating new HTTP connection");

        let http_config = Configuration {
            timeout: Some(inner.config.timeout),
            buffer_size: Some(inner.config.buffer_size),
            buffer_size_tx: Some(inner.config.buffer_size),
            // 🔧 TLS Configuration for HTTPS support
            // Enable certificate bundle for server verification
            // This should resolve the "No server verification option set" error
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            ..Default::default()
        };

        let connection = EspHttpConnection::new(&http_config)?;
        debug!("HTTP connection established with TLS certificate bundle");

        Ok(connection)
    }

    /// Perform a POST request with JSON payload
    pub async fn post<T: Serialize>(&self, endpoint: &str, payload: &T) -> Result<ResponseData> {
        let url = self.build_url(endpoint).await;
        let json_payload = serde_json::to_string(payload)?;

        debug!("POST request to: {}", url);
        debug!("Payload: {}", json_payload);

        let connection = self.get_connection().await?;
        let headers = self.build_headers_with_auth().await;

        self.send_request(
            connection,
            Method::Post,
            &url,
            Some(json_payload.as_bytes()),
            &headers,
        )
        .await
    }

    /// Perform a GET request
    pub async fn get(&self, endpoint: &str) -> Result<ResponseData> {
        let url = self.build_url(endpoint).await;

        debug!("GET request to: {}", url);

        let connection = self.get_connection().await?;
        let headers = self.build_headers_with_auth().await;

        self.send_request(connection, Method::Get, &url, None, &headers)
            .await
    }

    /// Perform a PUT request with JSON payload
    pub async fn put<T: Serialize>(&self, endpoint: &str, payload: &T) -> Result<ResponseData> {
        let url = self.build_url(endpoint).await;
        let json_payload = serde_json::to_string(payload)?;

        debug!("PUT request to: {}", url);
        debug!("Payload: {}", json_payload);

        let connection = self.get_connection().await?;
        let headers = self.build_headers_with_auth().await;

        self.send_request(
            connection,
            Method::Put,
            &url,
            Some(json_payload.as_bytes()),
            &headers,
        )
        .await
    }

    /// Perform a DELETE request
    pub async fn delete(&self, endpoint: &str) -> Result<ResponseData> {
        let url = self.build_url(endpoint).await;

        debug!("DELETE request to: {}", url);

        let connection = self.get_connection().await?;
        let headers = self.build_headers_with_auth().await;

        self.send_request(connection, Method::Delete, &url, None, &headers)
            .await
    }

    /// Send the actual HTTP request and process the response
    async fn send_request(
        &self,
        connection: EspHttpConnection,
        method: Method,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(String, String)],
    ) -> Result<ResponseData> {
        use embedded_svc::http::client::Client;

        // Wrap the connection in a client
        let mut client = Client::wrap(connection);

        // Convert headers to the required format
        let header_refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        // Create request based on method with headers
        let mut request = match method {
            Method::Get => client.get(url)?,
            Method::Post => client.post(url, &header_refs)?,
            Method::Put => client.put(url, &header_refs)?,
            Method::Delete => client.delete(url)?,
            _ => return Err(anyhow!("Unsupported HTTP method: {:?}", method)),
        };

        // Write request body if provided
        if let Some(body_data) = body {
            request.write_all(body_data)?;
        }

        // Send the request and get the response
        let mut response = request.submit()?;
        let status = response.status();

        debug!("Response status: {}", status);

        // Collect response headers
        let headers_vec = Vec::new();
        // Note: ESP-IDF HTTP client doesn't provide easy access to response headers
        // This is a limitation of the current implementation

        // Read response body
        let mut body_buffer = vec![0u8; 4096];
        let mut body_content = String::new();
        let mut total_read = 0;

        loop {
            match response.read(&mut body_buffer) {
                Ok(0) => break, // EOF
                Ok(bytes_read) => {
                    total_read += bytes_read;
                    if let Ok(chunk) = std::str::from_utf8(&body_buffer[..bytes_read]) {
                        body_content.push_str(chunk);
                    }

                    // Prevent infinite loops with very large responses
                    if total_read > 1024 * 1024 {
                        warn!("Response body too large, truncating at 1MB");
                        break;
                    }
                }
                Err(e) => {
                    error!("Error reading response body: {:?}", e);
                    break;
                }
            }
        }

        let response_data = ResponseData {
            status_code: status,
            headers: headers_vec,
            body: body_content,
        };

        // Check if the response status indicates success
        if status >= 200 && status < 300 {
            debug!("Request successful: {}", status);
            Ok(response_data)
        } else {
            error!("Request failed with status: {}", status);
            Err(anyhow!(
                "HTTP request failed with status {}: {}",
                status,
                response_data.body
            ))
        }
    }

    /// Build the complete URL from base URL and endpoint
    async fn build_url(&self, endpoint: &str) -> String {
        let inner = self.inner.lock().await;
        let base_url = &inner.base_url;

        if endpoint.starts_with('/') {
            format!("{}{}", base_url.trim_end_matches('/'), endpoint)
        } else {
            format!("{}/{}", base_url.trim_end_matches('/'), endpoint)
        }
    }

    /// Build HTTP headers with authorization token (internal use)
    async fn build_headers_with_auth(&self) -> Vec<(String, String)> {
        let inner = self.inner.lock().await;
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
            ("User-Agent".to_string(), "ESP32-IoT-Device/1.0".to_string()),
        ];

        // Add authorization header if token is set
        if let Some(ref token) = inner.token {
            headers.push(("Authorization".to_string(), format!("Bearer {}", token)));
        }

        headers
    }

    /// Example method: Register a device with the API
    pub async fn register_device(
        &self,
        device_info: &DeviceRegistration,
    ) -> Result<DeviceRegistrationResponse> {
        let response = self.post("/devices/register", device_info).await?;

        // Parse the response JSON
        let registration_response: DeviceRegistrationResponse =
            serde_json::from_str(&response.body)?;

        info!(
            "Device registered successfully: {}",
            registration_response.device_id
        );
        Ok(registration_response)
    }

    /// Example method: Get device status
    pub async fn get_device_status(&self, device_id: &str) -> Result<DeviceStatus> {
        let endpoint = format!("/devices/{}/status", device_id);
        let response = self.get(&endpoint).await?;

        // Parse the response JSON
        let status: DeviceStatus = serde_json::from_str(&response.body)?;

        debug!("Device status retrieved: {:?}", status);
        Ok(status)
    }

    /// Example method: Update device configuration
    pub async fn update_device_config(&self, device_id: &str, config: &DeviceConfig) -> Result<()> {
        let endpoint = format!("/devices/{}/config", device_id);
        let _response = self.put(&endpoint, config).await?;

        info!("Device configuration updated successfully");
        Ok(())
    }

    /// Example method: Send telemetry data
    pub async fn send_telemetry(&self, device_id: &str, telemetry: &TelemetryData) -> Result<()> {
        let endpoint = format!("/devices/{}/telemetry", device_id);
        let _response = self.post(&endpoint, telemetry).await?;

        debug!("Telemetry data sent successfully");
        Ok(())
    }

    /// Health check endpoint
    pub async fn health_check(&self) -> Result<HealthStatus> {
        let response = self.get("/health").await?;

        let health: HealthStatus = serde_json::from_str(&response.body)?;
        Ok(health)
    }
}

// Example data structures for API endpoints

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRegistration {
    pub device_id: String,
    pub device_type: String,
    pub firmware_version: String,
    pub hardware_version: String,
    pub mac_address: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRegistrationResponse {
    pub device_id: String,
    pub registration_token: String,
    pub server_endpoints: Vec<String>,
    pub polling_interval: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub device_id: String,
    pub status: String,
    pub last_seen: String,
    pub uptime: u64,
    pub wifi_rssi: i32,
    pub free_memory: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub polling_interval: u32,
    pub log_level: String,
    pub features: Vec<String>,
    pub settings: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryData {
    pub device_id: String,
    pub timestamp: u64,
    pub temperature: Option<f32>,
    pub humidity: Option<f32>,
    pub battery_level: Option<f32>,
    pub sensor_data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub timestamp: u64,
    pub version: String,
}

// Convenience functions for common use cases

/// Create a singleton API client instance
pub fn create_api_client(base_url: String) -> ApiClient {
    ApiClient::new(base_url)
}

/// Create a configured API client with authentication
pub async fn create_authenticated_client(base_url: String, token: String) -> ApiClient {
    ApiClient::new(base_url).with_token(token).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_api_client_creation() {
        let client = ApiClient::new("https://api.example.com".to_string());

        // Test that the client can be cloned (Arc behavior)
        let _client_clone = client.clone();

        // Test token setting
        let client_with_token = client.with_token("test_token".to_string()).await;
        // Additional tests would go here in a real implementation
    }

    #[tokio::test]
    async fn test_url_building() {
        let client = ApiClient::new("https://api.example.com".to_string());

        let url1 = client.build_url("/devices/register").await;
        assert_eq!(url1, "https://api.example.com/devices/register");

        let url2 = client.build_url("devices/register").await;
        assert_eq!(url2, "https://api.example.com/devices/register");
    }
}
