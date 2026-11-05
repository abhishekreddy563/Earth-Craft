//! Authenticated access to the Earth Pulse sidecar. No cloud credentials enter the desktop.
use image::RgbaImage;
use reqwest::{Client, Method};
use serde_json::Value;
use std::{env, time::Duration};

const BASE: &str = "http://127.0.0.1:8765";
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct AnalysisClient {
    client: Client,
    token: String,
}
impl AnalysisClient {
    pub fn connect() -> Result<Self, String> {
        let token = env::var("EARTH_PULSE_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or("Start EarthCraft using the Earth Pulse launcher to connect to its backend.")?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client, token })
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Vec<u8>, String> {
        if !path.starts_with('/') || path.starts_with("//") {
            return Err("Unexpected backend path.".into());
        }
        let mut request = self
            .client
            .request(method, format!("{BASE}{path}"))
            .bearer_auth(&self.token);
        if path == "/verify" {
            request = request.timeout(Duration::from_secs(1320));
        }
        if let Some(value) = body {
            request = request.json(&value);
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| format!("Local backend request failed: {e}"))?;
        let status = response.status();
        let mut data = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            if data.len() + chunk.len() > RESPONSE_LIMIT {
                return Err("Backend response is too large.".into());
            }
            data.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let error = serde_json::from_slice::<Value>(&data)
                .ok()
                .and_then(|v| v["detail"].as_str().map(str::to_owned));
            return Err(error.unwrap_or_else(|| format!("Backend returned HTTP {status}.")));
        }
        Ok(data)
    }

    pub async fn json(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let method = Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let bytes = self.request(method, path, body).await?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid backend JSON: {e}"))
    }

    pub async fn tile(&self, path: &str) -> Result<RgbaImage, String> {
        if !path.starts_with("/results/") {
            return Err("Unexpected layer endpoint.".into());
        }
        let data = self.request(Method::GET, path, None).await?;
        tokio::task::spawn_blocking(move || {
            let reader = image::ImageReader::with_format(
                std::io::Cursor::new(&data),
                image::ImageFormat::Png,
            );
            let (w, h) = reader.into_dimensions().map_err(|e| e.to_string())?;
            if w > 512 || h > 512 {
                return Err("Invalid analysis tile dimensions.".into());
            }
            image::load_from_memory_with_format(&data, image::ImageFormat::Png)
                .map(|i| i.into_rgba8())
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}
