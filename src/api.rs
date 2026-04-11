use std::{
    fmt::Write,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::debug;

use crate::config::GfroerliConfig;

/// User-Agent header sent with API requests.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
/// Timeout for individual API requests.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long cached sensor data remains valid.
const CACHE_TTL: Duration = Duration::from_secs(180);

/// A sensor as returned by the Gfrörli API.
#[derive(Debug, Clone, Deserialize)]
pub struct Sensor {
    pub id: u32,
    pub device_name: String,
    pub caption: Option<String>,
    pub latest_temperature: Option<f64>,
    pub latest_measurement_at: Option<String>,
}

impl Sensor {
    /// Return the caption with newlines collapsed to spaces, if present.
    fn caption_without_newlines(&self) -> Option<String> {
        self.caption
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(|c| c.split('\n').collect::<Vec<_>>().join(" "))
    }

    /// Format sensor as a line in the sensor list.
    pub fn format_list_entry(&self) -> String {
        match self.caption_without_newlines() {
            Some(caption) => {
                format!("#{}: {} ({})", self.id, self.device_name, caption)
            }
            _ => format!("#{}: {}", self.id, self.device_name),
        }
    }

    /// Format the current temperature reading.
    pub fn format_temperature(&self) -> String {
        let name = &self.device_name;
        match (self.latest_temperature, &self.latest_measurement_at) {
            (Some(temp), Some(time)) => {
                format!("{name}: {temp:.1}°C (measured at {time})")
            }
            (Some(temp), None) => {
                format!("{name}: {temp:.1}°C")
            }
            _ => {
                format!("{name}: no recent measurement available")
            }
        }
    }
}

/// Time-limited cache of the sensor list fetched from the API.
struct CachedSensors {
    /// The cached sensor data.
    sensors: Vec<Sensor>,
    /// When the data was fetched, used to determine cache expiry.
    fetched_at: Instant,
}

/// Client for the Gfrörli REST API with built-in caching.
pub struct GfroerliClient {
    http: reqwest::Client,
    config: GfroerliConfig,
    cache: Arc<RwLock<Option<CachedSensors>>>,
}

impl GfroerliClient {
    pub fn new(config: GfroerliConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("failed to build HTTP client"),
            config,
            cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Validate the API key by making a test request to the API.
    ///
    /// Should be called once at startup to fail early if the key is invalid.
    pub async fn validate_api_key(&self) -> anyhow::Result<()> {
        let url = format!("{}/api/sensors", self.config.api_url);
        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.config.api_key)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!(
                "Gfrörli API key is invalid (401 Unauthorized). \
                 Check the api_key in your config or GFROERLI_BOT__GFROERLI__API_KEY env var."
            );
        }
        response.error_for_status()?;
        Ok(())
    }

    /// Fetch all sensors, using the cache if still valid.
    pub async fn sensors(&self) -> anyhow::Result<Vec<Sensor>> {
        // Check cache
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.as_ref()
                && cached.fetched_at.elapsed() < CACHE_TTL
            {
                return Ok(cached.sensors.clone());
            }
        }

        // Cache miss or expired — fetch from API
        debug!("Fetching sensors from API");
        let url = format!("{}/api/mobile_app/sensors", self.config.api_url);
        let sensors: Vec<Sensor> = self
            .http
            .get(&url)
            .bearer_auth(&self.config.api_key)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            *cache = Some(CachedSensors {
                sensors: sensors.clone(),
                fetched_at: Instant::now(),
            });
        }

        Ok(sensors)
    }

    /// Find sensors matching a query (by ID or name substring).
    pub async fn find_sensors(&self, query: &str) -> anyhow::Result<Vec<Sensor>> {
        let sensors = self.sensors().await?;

        // Try parsing as sensor ID first
        if let Ok(id) = query.parse::<u32>() {
            let matches: Vec<Sensor> = sensors.into_iter().filter(|s| s.id == id).collect();
            return Ok(matches);
        }

        // Case-insensitive substring match against device_name
        let query_lower = query.to_lowercase();
        let matches: Vec<Sensor> = sensors
            .into_iter()
            .filter(|s| s.device_name.to_lowercase().contains(&query_lower))
            .collect();

        Ok(matches)
    }

    /// Format the full sensor list as a text message.
    pub async fn format_sensor_list(&self) -> anyhow::Result<String> {
        let sensors = self.sensors().await?;
        if sensors.is_empty() {
            return Ok("No sensors found.".to_string());
        }

        let mut output = String::from("Available sensors:\n\n");
        for sensor in &sensors {
            writeln!(output, "{}", sensor.format_list_entry()).unwrap();
        }
        output.truncate(output.trim_end().len());
        Ok(output)
    }
}
