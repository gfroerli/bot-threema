use std::{
    fmt::Write,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};
use tokio::sync::RwLock;
use tracing::debug;

use crate::config::GfroerliConfig;

/// User-Agent header sent with API requests.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
/// Timeout for individual API requests.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// How long cached sensor data remains valid.
const CACHE_TTL: Duration = Duration::from_secs(180);

/// A sensor as returned by the Gfrörli API.
#[derive(Debug, Clone, Deserialize)]
pub struct Sensor {
    pub id: u32,
    pub device_name: String,
    pub latest_temperature: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_timestamp")]
    pub latest_measurement_at: Option<DateTime<Utc>>,
}

/// Deserialize an optional Unix timestamp (seconds) into `Option<DateTime<Utc>>`.
fn deserialize_optional_timestamp<'de, D>(
    deserializer: D,
) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<i64>::deserialize(deserializer)?
        .map(|ts| {
            DateTime::from_timestamp(ts, 0)
                .ok_or_else(|| serde::de::Error::custom(format!("invalid timestamp: {ts}")))
        })
        .transpose()
}

/// Format a timestamp as a human-readable relative time string.
fn format_relative_time(time: DateTime<Utc>) -> String {
    let seconds = (Utc::now() - time).num_seconds().max(0);
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    match () {
        _ if seconds < 60 => "a few seconds ago".to_string(),
        _ if minutes == 1 => "1 minute ago".to_string(),
        _ if minutes < 60 => format!("{minutes} minutes ago"),
        _ if hours == 1 => "1 hour ago".to_string(),
        _ if hours < 24 => format!("{hours} hours ago"),
        _ if days == 1 => "1 day ago".to_string(),
        _ => format!("{days} days ago"),
    }
}

impl Sensor {
    /// Format sensor as a line in the sensor list.
    pub fn format_list_entry(&self) -> String {
        match self.latest_temperature {
            Some(temp) => format!(
                "{} \u{2013} {temp:.1}°C (#{self_id})",
                self.device_name,
                self_id = self.id
            ),
            None => format!("{} (#{self_id})", self.device_name, self_id = self.id),
        }
    }

    /// Format the current temperature reading.
    pub fn format_temperature(&self) -> String {
        let name = &self.device_name;
        match (self.latest_temperature, self.latest_measurement_at) {
            (Some(temp), Some(time)) => {
                let relative = format_relative_time(time);
                format!("{name}: {temp:.1}°C ({relative})")
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
            .head(&url)
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
        let mut sensors = self.sensors().await?;
        if sensors.is_empty() {
            return Ok("No sensors found.".to_string());
        }

        sensors.sort_by(|a, b| a.device_name.cmp(&b.device_name));

        let mut output = String::from("Available sensors:\n\n");
        for sensor in &sensors {
            writeln!(output, "{}", sensor.format_list_entry()).unwrap();
        }
        output.truncate(output.trim_end().len());
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, TimeZone, Utc};
    use rstest::rstest;

    use super::*;

    fn make_sensor(id: u32, name: &str, temp: Option<f64>, time: Option<DateTime<Utc>>) -> Sensor {
        Sensor {
            id,
            device_name: name.to_string(),
            latest_temperature: temp,
            latest_measurement_at: time,
        }
    }

    mod format_list_entry {
        use super::*;

        #[test]
        fn with_temperature() {
            let sensor = make_sensor(13, "Bennau, Alp", Some(34.5), None);
            assert_eq!(
                sensor.format_list_entry(),
                "Bennau, Alp \u{2013} 34.5°C (#13)"
            );
        }

        #[test]
        fn without_temperature() {
            let sensor = make_sensor(42, "Aare Bern", None, None);
            assert_eq!(sensor.format_list_entry(), "Aare Bern (#42)");
        }
    }

    mod format_temperature {
        use super::*;

        #[test]
        fn with_temp_and_time() {
            let time = Utc::now() - TimeDelta::hours(2);
            let sensor = make_sensor(1, "Aare Bern", Some(18.3), Some(time));
            assert_eq!(
                sensor.format_temperature(),
                "Aare Bern: 18.3°C (2 hours ago)"
            );
        }

        #[test]
        fn with_temp_no_time() {
            let sensor = make_sensor(1, "Aare Bern", Some(18.3), None);
            insta::assert_snapshot!(sensor.format_temperature());
        }

        #[test]
        fn no_measurement() {
            let sensor = make_sensor(1, "Aare Bern", None, None);
            insta::assert_snapshot!(sensor.format_temperature());
        }

        #[test]
        fn rounds_to_one_decimal() {
            let sensor = make_sensor(1, "Aare Bern", Some(18.347), None);
            insta::assert_snapshot!(sensor.format_temperature());
        }
    }

    mod format_relative_time {
        use super::*;

        #[rstest]
        #[case(0, "a few seconds ago")]
        #[case(30, "a few seconds ago")]
        #[case(60, "1 minute ago")]
        #[case(300, "5 minutes ago")]
        #[case(3540, "59 minutes ago")]
        #[case(3600, "1 hour ago")]
        #[case(10800, "3 hours ago")]
        #[case(82800, "23 hours ago")]
        #[case(86400, "1 day ago")]
        #[case(604800, "7 days ago")]
        fn relative_time(#[case] seconds_ago: i64, #[case] expected: &str) {
            let time = Utc::now() - TimeDelta::seconds(seconds_ago);
            assert_eq!(super::super::format_relative_time(time), expected);
        }
    }

    mod deserialize_sensor {
        use super::*;

        #[test]
        fn valid_timestamp() {
            let json = r#"{"id": 1, "device_name": "Aare Bern", "latest_temperature": 18.3, "latest_measurement_at": 1752589800}"#;
            let sensor: Sensor = serde_json::from_str(json).unwrap();
            assert_eq!(sensor.id, 1);
            assert_eq!(sensor.device_name, "Aare Bern");
            assert_eq!(sensor.latest_temperature, Some(18.3));
            assert_eq!(
                sensor.latest_measurement_at,
                Some(Utc.with_ymd_and_hms(2025, 7, 15, 14, 30, 0).unwrap())
            );
        }

        #[test]
        fn null_timestamp() {
            let json = r#"{"id": 1, "device_name": "Aare Bern", "latest_temperature": null, "latest_measurement_at": null}"#;
            let sensor: Sensor = serde_json::from_str(json).unwrap();
            assert_eq!(sensor.latest_temperature, None);
            assert_eq!(sensor.latest_measurement_at, None);
        }

        #[test]
        fn missing_optional_fields() {
            let json = r#"{"id": 1, "device_name": "Aare Bern"}"#;
            let sensor: Sensor = serde_json::from_str(json).unwrap();
            assert_eq!(sensor.latest_temperature, None);
            assert_eq!(sensor.latest_measurement_at, None);
        }

        #[test]
        fn ignores_unknown_fields() {
            let json = r#"{"id": 1, "device_name": "Aare Bern", "caption": "Some caption", "extra": true}"#;
            let sensor: Sensor = serde_json::from_str(json).unwrap();
            assert_eq!(sensor.id, 1);
        }
    }
}
