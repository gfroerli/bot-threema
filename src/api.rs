use std::{
    fmt::Write,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Deserializer};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::config::GfroerliConfig;

/// User-Agent header sent with API requests.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
/// Timeout for individual API requests.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// How long cached sensor data remains valid.
const CACHE_TTL: Duration = Duration::from_secs(180);
/// Log API response times at warn level if they exceed this threshold.
const SLOW_REQUEST_THRESHOLD: Duration = Duration::from_secs(2);

/// Log the duration of an API request, at warn level if slow.
fn log_request_duration(endpoint: &str, elapsed: Duration) {
    if elapsed >= SLOW_REQUEST_THRESHOLD {
        warn!("{endpoint} completed in {elapsed:.1?}");
    } else {
        debug!("{endpoint} completed in {elapsed:.1?}");
    }
}

/// A sensor as returned by the Gfrörli API.
#[derive(Debug, Clone, Deserialize)]
pub struct Sensor {
    pub id: u32,
    pub device_name: String,
    #[serde(default)]
    pub caption: Option<String>,
    pub latest_temperature: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_timestamp")]
    pub latest_measurement_at: Option<DateTime<Utc>>,
    /// All-time highest temperature recorded at this sensor. Only populated
    /// by the per-sensor detail endpoint; `None` in the list response.
    #[serde(default)]
    pub maximum_temperature: Option<f64>,
}

/// A sponsor of the Gfrörli project.
#[derive(Debug, Clone, Deserialize)]
pub struct Sponsor {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub sponsor_type: SponsorType,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// IDs of the sensors backed by this sponsor. Populated by the
    /// `/api/sponsors` index; not returned by the mobile_app sponsor endpoint.
    #[serde(default)]
    pub sensor_ids: Vec<u32>,
}

/// The relationship a sponsor has to the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SponsorType {
    Sponsor,
    PublicDataProvider,
    Partner,
    #[serde(other)]
    Unknown,
}

impl SponsorType {
    /// The phrase used in `/sponsor <sensor>` output. "Sponsored by" for
    /// actual sponsors, "powered by" for everything else.
    pub fn preposition(self) -> &'static str {
        match self {
            SponsorType::Sponsor => "sponsored by",
            _ => "powered by",
        }
    }
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

/// A daily temperature aggregate as returned by the Gfrörli API.
#[derive(Debug, Clone, Deserialize)]
pub struct DailyTemperature {
    pub aggregation_date: NaiveDate,
    pub minimum_temperature: f64,
    pub maximum_temperature: f64,
    pub average_temperature: f64,
}

/// An hourly temperature aggregate as returned by the Gfrörli API.
#[derive(Debug, Clone, Deserialize)]
pub struct HourlyTemperature {
    pub aggregation_date: NaiveDate,
    pub aggregation_hour: u8,
    pub minimum_temperature: f64,
    pub maximum_temperature: f64,
    pub average_temperature: f64,
}

/// Filter sensors by query (ID or case-insensitive name substring).
fn filter_sensors(sensors: Vec<Sensor>, query: &str) -> Vec<Sensor> {
    // Try parsing as sensor ID first
    if let Ok(id) = query.parse::<u32>() {
        return sensors.into_iter().filter(|s| s.id == id).collect();
    }

    // Case-insensitive substring match against device_name
    let query_lower = query.to_lowercase();
    sensors
        .into_iter()
        .filter(|s| s.device_name.to_lowercase().contains(&query_lower))
        .collect()
}

/// Format a list of sensors as a human-readable text message.
fn format_sensor_list_text(mut sensors: Vec<Sensor>) -> String {
    if sensors.is_empty() {
        return "No sensors found.".to_string();
    }

    sensors.sort_by(|a, b| a.device_name.cmp(&b.device_name));

    let mut output = String::from("Available sensors:\n\n");
    for sensor in &sensors {
        writeln!(output, "{}", sensor.format_list_entry()).unwrap();
    }
    output.truncate(output.trim_end().len());
    output
}

/// Format the `/sponsors` list: filter to actual sponsors (those with at
/// least one linked sensor) and sort by sensor count (desc), then by
/// `created_at` (asc).
pub fn format_sponsor_list_text(sponsors: Vec<Sponsor>) -> String {
    let mut sponsors: Vec<Sponsor> = sponsors
        .into_iter()
        .filter(|s| s.sponsor_type == SponsorType::Sponsor && !s.sensor_ids.is_empty())
        .collect();

    if sponsors.is_empty() {
        return "No sponsors to list.".to_string();
    }

    sponsors.sort_by(|a, b| {
        b.sensor_ids
            .len()
            .cmp(&a.sensor_ids.len())
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut output =
        String::from("Thanks to the following sponsors for supporting the Gfrörli project:\n\n");
    for sponsor in &sponsors {
        let count = sponsor.sensor_ids.len();
        let unit = if count == 1 { "sensor" } else { "sensors" };
        writeln!(output, "- {} (_{count} {unit}_)", sponsor.name).unwrap();
    }
    output.truncate(output.trim_end().len());
    output
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

/// Map a temperature to a semantic face emoji for the sensor list.
///
/// Buckets mirror the Android app's temperature thresholds.
fn temperature_emoji(temp: Option<f64>) -> &'static str {
    match temp {
        None => "❓",
        Some(t) if t < 10.0 => "🥶",
        Some(t) if t <= 18.0 => "😨",
        Some(t) if t <= 21.0 => "😌",
        Some(t) if t <= 24.0 => "😎",
        Some(_) => "🥵",
    }
}

impl Sensor {
    /// Format sensor as a line in the sensor list.
    pub fn format_list_entry(&self) -> String {
        let name = &self.device_name;
        let id = self.id;
        match self.latest_temperature {
            Some(temp) => {
                let emoji = temperature_emoji(Some(temp));
                format!("{name} (#{id}) \u{2013} {temp:.1}°C {emoji}")
            }
            None => format!("{name} (#{id})"),
        }
    }

    /// Format the current temperature reading, prefixed with the sensor name.
    pub fn format_temperature(&self) -> String {
        format!(
            "{}: {}",
            self.device_name,
            self.format_temperature_reading()
        )
    }

    /// Format just the current temperature reading (without the sensor name),
    /// e.g. `"18.3°C 😎 (5 minutes ago)"`.
    pub fn format_temperature_reading(&self) -> String {
        match (self.latest_temperature, self.latest_measurement_at) {
            (Some(temp), Some(time)) => {
                let emoji = temperature_emoji(Some(temp));
                let relative = format_relative_time(time);
                format!("*{temp:.1}°C* {emoji} (_{relative}_)")
            }
            (Some(temp), None) => {
                let emoji = temperature_emoji(Some(temp));
                format!("*{temp:.1}°C* {emoji}")
            }
            _ => "no recent measurement available".to_string(),
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
        let endpoint = "/api/sponsors";
        let url = format!("{}{endpoint}", self.config.api_url);

        let start = Instant::now();
        let response = self
            .http
            .head(&url)
            .bearer_auth(&self.config.api_key)
            .send()
            .await?;
        log_request_duration(&format!("HEAD {endpoint}"), start.elapsed());

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
        let start = Instant::now();
        let sensors: Vec<Sensor> = self
            .http
            .get(&url)
            .bearer_auth(&self.config.api_key)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        log_request_duration("GET /api/mobile_app/sensors", start.elapsed());

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
        Ok(filter_sensors(sensors, query))
    }

    /// Fetch full details for a single sensor, including the all-time
    /// maximum temperature.
    pub async fn sensor_details(&self, sensor_id: u32) -> anyhow::Result<Sensor> {
        let endpoint = format!("/api/mobile_app/sensors/{sensor_id}");
        let url = format!("{}{endpoint}", self.config.api_url);

        let start = Instant::now();
        let sensor: Sensor = self
            .http
            .get(&url)
            .bearer_auth(&self.config.api_key)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        log_request_duration(&format!("GET {endpoint}"), start.elapsed());

        Ok(sensor)
    }

    /// Fetch all sponsors from the CRUD index endpoint.
    pub async fn sponsors(&self) -> anyhow::Result<Vec<Sponsor>> {
        let endpoint = "/api/sponsors";
        let url = format!("{}{endpoint}", self.config.api_url);

        let start = Instant::now();
        let sponsors: Vec<Sponsor> = self
            .http
            .get(&url)
            .bearer_auth(&self.config.api_key)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        log_request_duration(&format!("GET {endpoint}"), start.elapsed());

        Ok(sponsors)
    }

    /// Fetch the sponsor associated with a given sensor, if any. Returns
    /// `Ok(None)` if the sensor has no sponsor (HTTP 404).
    pub async fn sensor_sponsor(&self, sensor_id: u32) -> anyhow::Result<Option<Sponsor>> {
        let endpoint = format!("/api/mobile_app/sensors/{sensor_id}/sponsor");
        let url = format!("{}{endpoint}", self.config.api_url);

        let start = Instant::now();
        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.config.api_key)
            .send()
            .await?;
        log_request_duration(&format!("GET {endpoint}"), start.elapsed());

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let sponsor: Sponsor = response.error_for_status()?.json().await?;
        Ok(Some(sponsor))
    }

    /// Fetch daily temperature aggregates for a sensor over a date range.
    pub async fn daily_temperatures(
        &self,
        sensor_id: u32,
        from: NaiveDate,
        to: NaiveDate,
        limit: u32,
    ) -> anyhow::Result<Vec<DailyTemperature>> {
        let endpoint = format!("/api/mobile_app/sensors/{sensor_id}/daily_temperatures");
        let url = format!("{}{endpoint}", self.config.api_url);

        let start = Instant::now();
        let result: Vec<DailyTemperature> = self
            .http
            .get(&url)
            .bearer_auth(&self.config.api_key)
            .query(&[
                ("from", from.to_string()),
                ("to", to.to_string()),
                ("limit", limit.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        log_request_duration(&format!("GET {endpoint}"), start.elapsed());

        Ok(result)
    }

    /// Fetch hourly temperature aggregates for a sensor over a date range.
    pub async fn hourly_temperatures(
        &self,
        sensor_id: u32,
        from: NaiveDate,
        to: NaiveDate,
        limit: u32,
    ) -> anyhow::Result<Vec<HourlyTemperature>> {
        let endpoint = format!("/api/mobile_app/sensors/{sensor_id}/hourly_temperatures");
        let url = format!("{}{endpoint}", self.config.api_url);

        let start = Instant::now();
        let result: Vec<HourlyTemperature> = self
            .http
            .get(&url)
            .bearer_auth(&self.config.api_key)
            .query(&[
                ("from", from.to_string()),
                ("to", to.to_string()),
                ("limit", limit.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        log_request_duration(&format!("GET {endpoint}"), start.elapsed());

        Ok(result)
    }

    /// Format the full sensor list as a text message.
    pub async fn format_sensor_list(&self) -> anyhow::Result<String> {
        let sensors = self.sensors().await?;
        Ok(format_sensor_list_text(sensors))
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
            caption: None,
            latest_temperature: temp,
            latest_measurement_at: time,
            maximum_temperature: None,
        }
    }

    fn make_sponsor(
        id: u32,
        name: &str,
        sponsor_type: SponsorType,
        created_at: Option<DateTime<Utc>>,
        sensor_ids: Vec<u32>,
    ) -> Sponsor {
        Sponsor {
            id,
            name: name.to_string(),
            description: None,
            sponsor_type,
            created_at,
            sensor_ids,
        }
    }

    mod format_list_entry {
        use super::*;

        #[test]
        fn with_temperature() {
            let sensor = make_sensor(13, "Bennau, Alp", Some(34.5), None);
            assert_eq!(
                sensor.format_list_entry(),
                "Bennau, Alp (#13) \u{2013} 34.5°C 🥵"
            );
        }

        #[test]
        fn without_temperature() {
            let sensor = make_sensor(42, "Aare Bern", None, None);
            assert_eq!(sensor.format_list_entry(), "Aare Bern (#42)");
        }
    }

    mod temperature_emoji {
        use super::*;

        #[test]
        fn unknown() {
            assert_eq!(temperature_emoji(None), "❓");
        }

        #[rstest]
        #[case(-5.0, "🥶")]
        #[case(9.9, "🥶")]
        #[case(10.0, "😨")]
        #[case(15.0, "😨")]
        #[case(18.0, "😨")]
        #[case(18.1, "😌")]
        #[case(20.0, "😌")]
        #[case(21.0, "😌")]
        #[case(21.1, "😎")]
        #[case(23.0, "😎")]
        #[case(24.0, "😎")]
        #[case(24.1, "🥵")]
        #[case(30.0, "🥵")]
        fn buckets(#[case] temp: f64, #[case] expected: &str) {
            assert_eq!(temperature_emoji(Some(temp)), expected);
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
                "Aare Bern: *18.3°C* 😌 (_2 hours ago_)"
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
            assert_eq!(format_relative_time(time), expected);
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

    mod deserialize_daily_temperature {
        use super::*;

        #[test]
        fn valid() {
            let json = r#"{"aggregation_date": "2025-07-15", "minimum_temperature": 17.2, "maximum_temperature": 21.5, "average_temperature": 19.1}"#;
            let daily: DailyTemperature = serde_json::from_str(json).unwrap();
            assert_eq!(
                daily.aggregation_date,
                NaiveDate::from_ymd_opt(2025, 7, 15).unwrap()
            );
            assert_eq!(daily.minimum_temperature, 17.2);
            assert_eq!(daily.maximum_temperature, 21.5);
            assert_eq!(daily.average_temperature, 19.1);
        }

        #[test]
        fn ignores_unknown_fields() {
            let json = r#"{"aggregation_date": "2025-07-15", "minimum_temperature": 17.2, "maximum_temperature": 21.5, "average_temperature": 19.1, "extra": "ignored"}"#;
            let daily: DailyTemperature = serde_json::from_str(json).unwrap();
            assert_eq!(daily.average_temperature, 19.1);
        }
    }

    mod deserialize_hourly_temperature {
        use super::*;

        #[test]
        fn valid() {
            let json = r#"{"aggregation_date": "2025-07-15", "aggregation_hour": 14, "minimum_temperature": 17.2, "maximum_temperature": 21.5, "average_temperature": 19.1}"#;
            let hourly: HourlyTemperature = serde_json::from_str(json).unwrap();
            assert_eq!(
                hourly.aggregation_date,
                NaiveDate::from_ymd_opt(2025, 7, 15).unwrap()
            );
            assert_eq!(hourly.aggregation_hour, 14);
            assert_eq!(hourly.average_temperature, 19.1);
        }
    }

    mod filter_sensors {
        use super::*;

        fn sample_sensors() -> Vec<Sensor> {
            vec![
                make_sensor(1, "Aare Bern", None, None),
                make_sensor(2, "Rhein Basel", None, None),
                make_sensor(3, "Aare Thun", None, None),
                make_sensor(42, "Limmat Zürich", None, None),
            ]
        }

        #[test]
        fn by_id() {
            let matches = filter_sensors(sample_sensors(), "2");
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].id, 2);
        }

        #[test]
        fn by_id_no_match() {
            let matches = filter_sensors(sample_sensors(), "999");
            assert!(matches.is_empty());
        }

        #[test]
        fn by_name_substring_case_insensitive() {
            let matches = filter_sensors(sample_sensors(), "aare");
            assert_eq!(matches.len(), 2);
            assert_eq!(matches[0].device_name, "Aare Bern");
            assert_eq!(matches[1].device_name, "Aare Thun");
        }

        #[test]
        fn by_name_no_match() {
            let matches = filter_sensors(sample_sensors(), "Nothing");
            assert!(matches.is_empty());
        }

        #[test]
        fn by_name_single_match() {
            let matches = filter_sensors(sample_sensors(), "Rhein");
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].device_name, "Rhein Basel");
        }

        #[test]
        fn prefers_id_over_name() {
            // Query "42" matches ID 42 exactly, not "Sensor 42"
            let sensors = vec![
                make_sensor(42, "Aare Bern", None, None),
                make_sensor(5, "Sensor 42 Test", None, None),
            ];
            let matches = filter_sensors(sensors, "42");
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].id, 42);
        }
    }

    mod format_sensor_list_text {
        use super::*;

        #[test]
        fn with_sensors_sorted_alphabetically() {
            let sensors = vec![
                make_sensor(1, "Rhein Basel", None, None),
                make_sensor(2, "Aare Bern", None, None),
                make_sensor(3, "Limmat Zürich", None, None),
            ];
            insta::assert_snapshot!(format_sensor_list_text(sensors));
        }

        #[test]
        fn empty() {
            assert_eq!(format_sensor_list_text(vec![]), "No sensors found.");
        }
    }

    mod deserialize_sponsor {
        use super::*;

        #[test]
        fn full_sponsor() {
            let json = r#"{
                "id": 1,
                "name": "Threema",
                "description": "Secure messaging",
                "sponsor_type": "sponsor",
                "created_at": "2024-01-15T10:30:00Z"
            }"#;
            let sponsor: Sponsor = serde_json::from_str(json).unwrap();
            assert_eq!(sponsor.id, 1);
            assert_eq!(sponsor.name, "Threema");
            assert_eq!(sponsor.description.as_deref(), Some("Secure messaging"));
            assert_eq!(sponsor.sponsor_type, SponsorType::Sponsor);
            assert_eq!(
                sponsor.created_at,
                Some(Utc.with_ymd_and_hms(2024, 1, 15, 10, 30, 0).unwrap())
            );
        }

        #[test]
        fn public_data_provider() {
            let json = r#"{"id": 2, "name": "MeteoSwiss", "sponsor_type": "public_data_provider"}"#;
            let sponsor: Sponsor = serde_json::from_str(json).unwrap();
            assert_eq!(sponsor.sponsor_type, SponsorType::PublicDataProvider);
            assert_eq!(sponsor.description, None);
            assert_eq!(sponsor.created_at, None);
        }

        #[test]
        fn partner() {
            let json = r#"{"id": 3, "name": "OST", "sponsor_type": "partner"}"#;
            let sponsor: Sponsor = serde_json::from_str(json).unwrap();
            assert_eq!(sponsor.sponsor_type, SponsorType::Partner);
        }

        #[test]
        fn unknown_type_falls_back() {
            let json = r#"{"id": 4, "name": "X", "sponsor_type": "future_thing"}"#;
            let sponsor: Sponsor = serde_json::from_str(json).unwrap();
            assert_eq!(sponsor.sponsor_type, SponsorType::Unknown);
        }
    }

    mod format_sponsor_list_text {
        use super::*;

        fn ts(year: i32, month: u32, day: u32) -> DateTime<Utc> {
            Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
        }

        #[test]
        fn empty() {
            assert_eq!(format_sponsor_list_text(vec![]), "No sponsors to list.");
        }

        #[test]
        fn singular_and_plural() {
            let sponsors = vec![
                make_sponsor(
                    1,
                    "Threema",
                    SponsorType::Sponsor,
                    Some(ts(2020, 1, 1)),
                    vec![10, 11, 12],
                ),
                make_sponsor(
                    2,
                    "OST",
                    SponsorType::Sponsor,
                    Some(ts(2021, 1, 1)),
                    vec![13],
                ),
            ];
            insta::assert_snapshot!(format_sponsor_list_text(sponsors));
        }

        #[test]
        fn sorts_by_count_desc_then_created_at_asc() {
            let sponsors = vec![
                make_sponsor(
                    1,
                    "Newer",
                    SponsorType::Sponsor,
                    Some(ts(2023, 1, 1)),
                    vec![13],
                ),
                make_sponsor(
                    2,
                    "Older",
                    SponsorType::Sponsor,
                    Some(ts(2020, 1, 1)),
                    vec![14],
                ),
                make_sponsor(
                    3,
                    "Top",
                    SponsorType::Sponsor,
                    Some(ts(2022, 1, 1)),
                    vec![10, 11, 12],
                ),
            ];
            insta::assert_snapshot!(format_sponsor_list_text(sponsors));
        }

        #[test]
        fn filters_non_sponsor_types() {
            let sponsors = vec![
                make_sponsor(
                    1,
                    "Threema",
                    SponsorType::Sponsor,
                    Some(ts(2020, 1, 1)),
                    vec![10],
                ),
                make_sponsor(
                    2,
                    "MeteoSwiss",
                    SponsorType::PublicDataProvider,
                    Some(ts(2019, 1, 1)),
                    vec![11],
                ),
                make_sponsor(
                    3,
                    "Partner",
                    SponsorType::Partner,
                    Some(ts(2018, 1, 1)),
                    vec![12],
                ),
            ];
            insta::assert_snapshot!(format_sponsor_list_text(sponsors));
        }

        #[test]
        fn excludes_zero_sensor_sponsors() {
            let sponsors = vec![
                make_sponsor(
                    1,
                    "Active",
                    SponsorType::Sponsor,
                    Some(ts(2020, 1, 1)),
                    vec![10],
                ),
                make_sponsor(
                    2,
                    "Ghost",
                    SponsorType::Sponsor,
                    Some(ts(2019, 1, 1)),
                    vec![],
                ),
            ];
            insta::assert_snapshot!(format_sponsor_list_text(sponsors));
        }
    }
}
