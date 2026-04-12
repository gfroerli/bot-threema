use std::{fmt::Write, sync::Arc};

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use threema_gateway_bot::{
    commands::{CommandStyle, Commands},
    server::handler::{
        Action, CommandType, HandlerError, HandlerResult, MessageContext, MessageHandler, Response,
        TypingHandle,
    },
};

use crate::{
    api::{DailyTemperature, GfroerliClient, HourlyTemperature, Sensor},
    chart::{self, DISPLAY_TIMEZONE, DailyPoint, HourlyPoint},
};

/// 30-day summary statistics built from a sequence of daily aggregates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DailyStats {
    min: f64,
    max: f64,
    avg: f64,
}

/// Compute min/max/avg across a slice of daily temperature aggregates.
///
/// Returns `None` if the slice is empty. `avg` is the mean of the daily
/// averages (not weighted by hours).
fn compute_daily_stats(daily: &[DailyTemperature]) -> Option<DailyStats> {
    if daily.is_empty() {
        return None;
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    for d in daily {
        if d.minimum_temperature < min {
            min = d.minimum_temperature;
        }
        if d.maximum_temperature > max {
            max = d.maximum_temperature;
        }
        sum += d.average_temperature;
    }
    Some(DailyStats {
        min,
        max,
        avg: sum / daily.len() as f64,
    })
}

/// Format the text shown above the `/details` chart image.
fn format_details_text(sensor: &Sensor, stats: Option<DailyStats>) -> String {
    let mut out = format!("{} (#{})", sensor.device_name, sensor.id);
    if let Some(temp) = sensor.latest_temperature {
        write!(out, "\nCurrent: {temp:.1}°C").unwrap();
    }
    match stats {
        Some(s) => {
            write!(
                out,
                "\n\nLast 30 days:\n  min  {:.1}°C\n  max  {:.1}°C\n  avg  {:.1}°C",
                s.min, s.max, s.avg
            )
            .unwrap();
        }
        None => {
            out.push_str("\n\nNo recent measurements available.");
        }
    }
    out
}

/// Resolve a query to a single sensor, or produce a user-facing message
/// describing why disambiguation is needed.
///
/// Used by both `/temp` and `/details`. The `command_hint` is injected into
/// the disambiguation footer (e.g. `"/temp 1"` or `"/details 1"`).
fn resolve_single_sensor(
    query: &str,
    matches: Vec<Sensor>,
    command_hint: &str,
) -> Result<Sensor, String> {
    match matches.len() {
        0 => Err(format!(
            "No sensor found matching \"{query}\".\n\nUse /sensors to list all available sensors."
        )),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            let mut msg = format!("Multiple sensors match \"{query}\":\n\n");
            for sensor in &matches {
                msg.push_str(&sensor.format_list_entry());
                msg.push('\n');
            }
            write!(
                msg,
                "\nPlease be more specific or use the sensor ID (e.g. {command_hint})."
            )
            .unwrap();
            Err(msg)
        }
    }
}

/// Convert API daily aggregates into chart points, anchored at noon in the
/// chart's display timezone.
fn daily_points(daily: &[DailyTemperature]) -> Vec<DailyPoint> {
    daily
        .iter()
        .filter_map(|d| {
            let datetime = d.aggregation_date.and_hms_opt(12, 0, 0)?.and_utc();
            Some(DailyPoint {
                x: datetime.with_timezone(&DISPLAY_TIMEZONE),
                min: d.minimum_temperature,
                max: d.maximum_temperature,
                avg: d.average_temperature,
            })
        })
        .collect()
}

/// Convert API hourly aggregates into chart points, keeping only those within
/// the last 24 hours. Timestamps are converted to the chart's display
/// timezone.
fn hourly_points(hourly: &[HourlyTemperature]) -> Vec<HourlyPoint> {
    let cutoff = Utc::now() - TimeDelta::hours(24);
    let mut points: Vec<HourlyPoint> = hourly
        .iter()
        .filter_map(|h| {
            let datetime = h
                .aggregation_date
                .and_hms_opt(u32::from(h.aggregation_hour), 0, 0)?
                .and_utc();
            (datetime >= cutoff).then(|| HourlyPoint {
                x: datetime.with_timezone(&DISPLAY_TIMEZONE),
                min: h.minimum_temperature,
                max: h.maximum_temperature,
                avg: h.average_temperature,
            })
        })
        .collect();
    points.sort_by_key(|p| p.x);
    points
}

/// Threema bot handler for the Gfrörli water temperature service.
pub struct GfroerliHandler {
    client: Arc<GfroerliClient>,
}

impl GfroerliHandler {
    pub fn new(client: Arc<GfroerliClient>) -> Self {
        Self { client }
    }

    /// Handle `/sensors`: list all available sensors.
    async fn handle_sensors(&self, typing: &TypingHandle) -> HandlerResult<Action> {
        // Start sending typing indicator
        typing.send();

        let text = self
            .client
            .format_sensor_list()
            .await
            .map_err(HandlerError::from)?;
        Ok(Action::Respond(vec![Response::text(text)]))
    }

    /// Handle `/temp <query>`: look up a sensor by name or ID and show its temperature.
    async fn handle_temp(&self, args: &str, typing: &TypingHandle) -> HandlerResult<Action> {
        // Validate query
        let query = args.trim();
        if query.is_empty() {
            return Ok(Action::Respond(vec![Response::text(
                "Please specify a sensor name or ID.\n\nExample: /temp Aare\nExample: /temp 1\n\nUse /sensors to list all available sensors.",
            )]));
        }

        // Start sending typing indicator
        typing.send();

        // Find matching sensors
        let matches = self
            .client
            .find_sensors(query)
            .await
            .map_err(HandlerError::from)?;

        let text = match resolve_single_sensor(query, matches, "/temp 1") {
            Ok(sensor) => sensor.format_temperature(),
            Err(msg) => msg,
        };
        Ok(Action::Respond(vec![Response::text(text)]))
    }

    /// Handle `/details <query>`: show 30-day stats plus a PNG with hourly and
    /// daily temperature charts.
    async fn handle_details(&self, args: &str, typing: &TypingHandle) -> HandlerResult<Action> {
        // Validate query
        let query = args.trim();
        if query.is_empty() {
            return Ok(Action::Respond(vec![Response::text(
                "Please specify a sensor name or ID.\n\nExample: /details Aare\nExample: /details 1\n\nUse /sensors to list all available sensors.",
            )]));
        }

        // Start sending typing indicator
        typing.send();

        // Resolve query to a single sensor
        let matches = self
            .client
            .find_sensors(query)
            .await
            .map_err(HandlerError::from)?;
        let sensor = match resolve_single_sensor(query, matches, "/details 1") {
            Ok(sensor) => sensor,
            Err(msg) => return Ok(Action::Respond(vec![Response::text(msg)])),
        };

        // Fetch daily (last 30 days) and hourly (yesterday + today for the
        // last 24h window) aggregates in parallel
        let today = Utc::now().date_naive();
        let daily_from = today - TimeDelta::days(30);
        let hourly_from = today - TimeDelta::days(1);
        let daily_fut = self
            .client
            .daily_temperatures(sensor.id, daily_from, today, 30);
        let hourly_fut = self
            .client
            .hourly_temperatures(sensor.id, hourly_from, today, 48);
        let (daily, hourly) =
            tokio::try_join!(daily_fut, hourly_fut).map_err(HandlerError::from)?;

        // Stats + text
        let stats = compute_daily_stats(&daily);
        let text = format_details_text(&sensor, stats);

        // Render chart PNG
        let hourly_chart = hourly_points(&hourly);
        let daily_chart = daily_points(&daily);
        let png = chart::render_sensor_charts(&sensor.device_name, &hourly_chart, &daily_chart)
            .map_err(HandlerError::from)?;

        Ok(Action::Respond(vec![Response::image(
            png,
            "image/png",
            Some(text),
        )]))
    }

    /// Handle `/about`: show information about the Gfrörli project.
    fn handle_about() -> Action {
        Action::Respond(vec![Response::text(
            "Gfrörli is a community project that measures water temperatures in Swiss water bodies.\n\n\
             Website: https://gfrör.li/\n\n\
             This bot allows you to quickly check current water temperatures directly from your phone. \
             Use /sensors to see all available measurement stations, or /temp to get the latest reading \
             for a specific sensor.",
        )])
    }
}

#[async_trait]
impl MessageHandler for GfroerliHandler {
    fn description(&self) -> Option<&str> {
        Some("Gfrörli Bot: Check water temperatures in Swiss water bodies.")
    }

    fn commands() -> Commands {
        Commands::new()
            .style(CommandStyle::Slash)
            .register("sensors", "List all available sensors")
            .register("temp", "Get current temperature (e.g. /temp Aare)")
            .register(
                "details",
                "Show stats and charts for a sensor (e.g. /details Aare)",
            )
            .register("about", "About the Gfrörli project")
    }

    async fn handle_text(
        &self,
        _ctx: &MessageContext,
        _text: &str,
        _typing: &TypingHandle,
    ) -> HandlerResult<Action> {
        Ok(Action::ShowHelp {
            prelude: Some("I didn't understand that. Here are the available commands:".into()),
        })
    }

    async fn handle_command(
        &self,
        _ctx: &MessageContext,
        command: &str,
        args: &str,
        _command_type: CommandType,
        typing: &TypingHandle,
    ) -> HandlerResult<Action> {
        match command {
            "sensors" => self.handle_sensors(typing).await,
            "temp" => self.handle_temp(args, typing).await,
            "details" => self.handle_details(args, typing).await,
            "about" => Ok(Self::handle_about()),
            _ => Ok(Action::ShowHelp { prelude: None }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod handle_about {
        use super::*;

        #[test]
        fn returns_project_info() {
            let action = GfroerliHandler::handle_about();
            let Action::Respond(responses) = action else {
                panic!("expected Action::Respond");
            };
            assert_eq!(responses.len(), 1);
            let Response::Text(text) = &responses[0] else {
                panic!("expected Response::Text");
            };
            insta::assert_snapshot!(text);
        }
    }

    fn make_sensor(id: u32, name: &str, temp: Option<f64>) -> Sensor {
        Sensor {
            id,
            device_name: name.to_string(),
            latest_temperature: temp,
            latest_measurement_at: None,
        }
    }

    mod resolve_single_sensor {
        use super::*;

        #[test]
        fn no_match() {
            let err = resolve_single_sensor("nonexistent", vec![], "/temp 1").unwrap_err();
            insta::assert_snapshot!(err);
        }

        #[test]
        fn single_match() {
            let sensors = vec![make_sensor(1, "Aare Bern", Some(18.3))];
            let sensor = resolve_single_sensor("Aare", sensors, "/temp 1").unwrap();
            assert_eq!(sensor.id, 1);
        }

        #[test]
        fn multiple_matches_temp_hint() {
            let sensors = vec![
                make_sensor(1, "Aare Bern", Some(18.3)),
                make_sensor(3, "Aare Thun", Some(17.1)),
            ];
            let err = resolve_single_sensor("Aare", sensors, "/temp 1").unwrap_err();
            insta::assert_snapshot!(err);
        }

        #[test]
        fn multiple_matches_details_hint() {
            let sensors = vec![
                make_sensor(1, "Aare Bern", Some(18.3)),
                make_sensor(3, "Aare Thun", Some(17.1)),
            ];
            let err = resolve_single_sensor("Aare", sensors, "/details 1").unwrap_err();
            insta::assert_snapshot!(err);
        }
    }

    mod compute_daily_stats {
        use chrono::NaiveDate;
        use rstest::rstest;

        use super::*;

        fn d(min: f64, max: f64, avg: f64) -> DailyTemperature {
            DailyTemperature {
                aggregation_date: NaiveDate::from_ymd_opt(2025, 7, 15).unwrap(),
                minimum_temperature: min,
                maximum_temperature: max,
                average_temperature: avg,
            }
        }

        #[test]
        fn empty() {
            assert_eq!(compute_daily_stats(&[]), None);
        }

        #[test]
        fn single() {
            let stats = compute_daily_stats(&[d(10.0, 20.0, 15.0)]).unwrap();
            assert_eq!(stats.min, 10.0);
            assert_eq!(stats.max, 20.0);
            assert_eq!(stats.avg, 15.0);
        }

        #[rstest]
        #[case(&[(10.0, 20.0, 15.0), (12.0, 22.0, 17.0)], 10.0, 22.0, 16.0)]
        #[case(&[(5.0, 8.0, 6.5), (9.0, 11.0, 10.0), (7.0, 15.0, 11.0)], 5.0, 15.0, 9.166666666666666)]
        fn multi(
            #[case] input: &[(f64, f64, f64)],
            #[case] expected_min: f64,
            #[case] expected_max: f64,
            #[case] expected_avg: f64,
        ) {
            let data: Vec<_> = input.iter().map(|(mn, mx, av)| d(*mn, *mx, *av)).collect();
            let stats = compute_daily_stats(&data).unwrap();
            assert_eq!(stats.min, expected_min);
            assert_eq!(stats.max, expected_max);
            assert!((stats.avg - expected_avg).abs() < 1e-9);
        }
    }

    mod format_details_text {
        use super::*;

        #[test]
        fn with_stats_and_current() {
            let sensor = make_sensor(1, "Aare Bern", Some(18.3));
            let stats = Some(DailyStats {
                min: 14.1,
                max: 22.4,
                avg: 18.7,
            });
            insta::assert_snapshot!(format_details_text(&sensor, stats));
        }

        #[test]
        fn without_stats() {
            let sensor = make_sensor(42, "Limmat Zürich", None);
            insta::assert_snapshot!(format_details_text(&sensor, None));
        }
    }
}
