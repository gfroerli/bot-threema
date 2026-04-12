use std::{fmt::Write, sync::Arc};

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use threema_gateway::ThreemaId;
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

/// Summary statistics (min/max/avg) built from a sequence of temperature
/// aggregates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TempStats {
    min: f64,
    max: f64,
    avg: f64,
}

/// Compute min/max/avg across an iterator of `(min, max, avg)` tuples.
///
/// Returns `None` if the iterator is empty. `avg` is the unweighted mean of
/// the per-item averages.
fn compute_stats<I>(iter: I) -> Option<TempStats>
where
    I: IntoIterator<Item = (f64, f64, f64)>,
{
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    let mut count = 0usize;
    for (mn, mx, av) in iter {
        if mn < min {
            min = mn;
        }
        if mx > max {
            max = mx;
        }
        sum += av;
        count += 1;
    }
    (count > 0).then_some(TempStats {
        min,
        max,
        avg: sum / count as f64,
    })
}

/// Format the text shown above the `/stats` chart image.
fn format_stats_text(
    sensor: &Sensor,
    stats_24h: Option<TempStats>,
    stats_30d: Option<TempStats>,
) -> String {
    let mut out = sensor.device_name.clone();
    if let Some(caption) = sensor.caption.as_deref().map(str::trim)
        && !caption.is_empty()
    {
        write!(out, ": _{caption}_").unwrap();
    }
    write!(out, "\n\n🌡️ *{}*", sensor.format_temperature_reading()).unwrap();
    if let Some(s) = stats_24h {
        write!(
            out,
            "\n\n_Over the last 24 hours, the temperature ranged from {:.1}°C to {:.1}°C, averaging {:.1}°C._",
            s.min, s.max, s.avg
        )
        .unwrap();
    }
    if let Some(s) = stats_30d {
        write!(
            out,
            "\n\n_Over the last 30 days, the temperature ranged from {:.1}°C to {:.1}°C, averaging {:.1}°C._",
            s.min, s.max, s.avg
        )
        .unwrap();
    }
    out
}

/// Resolve a query to a single sensor, or produce a user-facing message
/// describing why disambiguation is needed.
///
/// Used by both `/temp` and `/stats`. The `command_hint` is injected into
/// the disambiguation footer (e.g. `"/temp 1"` or `"/stats 1"`).
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
    maintainer_ids: Vec<ThreemaId>,
}

impl GfroerliHandler {
    pub fn new(client: Arc<GfroerliClient>, maintainer_ids: Vec<ThreemaId>) -> Self {
        Self {
            client,
            maintainer_ids,
        }
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

    /// Handle `/stats <query>`: show 30-day stats plus a PNG with hourly and
    /// daily temperature charts.
    async fn handle_stats(&self, args: &str, typing: &TypingHandle) -> HandlerResult<Action> {
        // Validate query
        let query = args.trim();
        if query.is_empty() {
            return Ok(Action::Respond(vec![Response::text(
                "Please specify a sensor name or ID.\n\nExample: /stats Aare\nExample: /stats 1\n\nUse /sensors to list all available sensors.",
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
        let sensor = match resolve_single_sensor(query, matches, "/stats 1") {
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

        // Convert aggregates into chart points (hourly filters to last 24h)
        let hourly_chart = hourly_points(&hourly);
        let daily_chart = daily_points(&daily);

        // Stats + text
        let stats_24h = compute_stats(hourly_chart.iter().map(|p| (p.min, p.max, p.avg)));
        let stats_30d = compute_stats(daily.iter().map(|d| {
            (
                d.minimum_temperature,
                d.maximum_temperature,
                d.average_temperature,
            )
        }));
        let text = format_stats_text(&sensor, stats_24h, stats_30d);
        let png = chart::render_sensor_charts(&sensor.device_name, &hourly_chart, &daily_chart)
            .map_err(HandlerError::from)?;

        Ok(Action::Respond(vec![Response::image(
            png,
            "image/png",
            Some(text),
        )]))
    }

    /// Handle `/about`: show information about the Gfrörli project.
    fn handle_about(&self) -> Action {
        Action::Respond(vec![Response::text(format_about_text(
            &self.maintainer_ids,
        ))])
    }
}

/// Build the text shown in response to `/about`.
///
/// Appends a maintainer contact section if `maintainer_ids` is non-empty; multiple
/// IDs are joined with ` or `.
fn format_about_text(maintainer_ids: &[ThreemaId]) -> String {
    let mut text = String::from(
        "Gfrörli is a community project that measures water temperatures in Swiss water bodies.\n\n\
         Website: https://gfrör.li/\n\n\
         This bot allows you to quickly check current water temperatures directly from your phone. \
         Use /sensors to see all available measurement stations, or /temp to get the latest reading \
         for a specific sensor.",
    );
    if !maintainer_ids.is_empty() {
        let links = maintainer_ids
            .iter()
            .map(|id| format!("https://threema.id/{id}"))
            .collect::<Vec<_>>()
            .join(" or ");
        write!(
            text,
            "\n\nIf you have any question about this bot, please feel free to contact {links}"
        )
        .unwrap();
    }
    text
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
                "stats",
                "Show stats and charts for a sensor (e.g. /stats Aare)",
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
            "stats" => self.handle_stats(args, typing).await,
            "about" => Ok(self.handle_about()),
            _ => Ok(Action::ShowHelp { prelude: None }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod format_about_text {
        use super::*;

        #[test]
        fn without_maintainers() {
            insta::assert_snapshot!(format_about_text(&[]));
        }

        #[test]
        fn with_single_maintainer() {
            insta::assert_snapshot!(format_about_text(&["AAAABBBB".try_into().unwrap()]));
        }

        #[test]
        fn with_multiple_maintainers() {
            insta::assert_snapshot!(format_about_text(&[
                "AAAABBBB".try_into().unwrap(),
                "CCCCDDDD".try_into().unwrap(),
                "EEEEFFFF".try_into().unwrap(),
            ]));
        }
    }

    fn make_sensor(id: u32, name: &str, temp: Option<f64>) -> Sensor {
        Sensor {
            id,
            device_name: name.to_string(),
            caption: None,
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
        fn multiple_matches_stats_hint() {
            let sensors = vec![
                make_sensor(1, "Aare Bern", Some(18.3)),
                make_sensor(3, "Aare Thun", Some(17.1)),
            ];
            let err = resolve_single_sensor("Aare", sensors, "/stats 1").unwrap_err();
            insta::assert_snapshot!(err);
        }
    }

    mod compute_stats {
        use rstest::rstest;

        use super::*;

        #[test]
        fn empty() {
            assert_eq!(compute_stats(std::iter::empty()), None);
        }

        #[test]
        fn single() {
            let stats = compute_stats([(10.0, 20.0, 15.0)]).unwrap();
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
            let stats = compute_stats(input.iter().copied()).unwrap();
            assert_eq!(stats.min, expected_min);
            assert_eq!(stats.max, expected_max);
            assert!((stats.avg - expected_avg).abs() < 1e-9);
        }
    }

    mod format_stats_text {
        use chrono::TimeDelta;

        use super::*;

        fn sensor_with_time(id: u32, name: &str, temp: Option<f64>, hours_ago: i64) -> Sensor {
            Sensor {
                id,
                device_name: name.to_string(),
                caption: None,
                latest_temperature: temp,
                latest_measurement_at: Some(Utc::now() - TimeDelta::hours(hours_ago)),
            }
        }

        #[test]
        fn with_stats_and_current() {
            let sensor = sensor_with_time(1, "Aare Bern", Some(18.3), 2);
            let stats_24h = Some(TempStats {
                min: 17.8,
                max: 19.2,
                avg: 18.5,
            });
            let stats_30d = Some(TempStats {
                min: 14.1,
                max: 22.4,
                avg: 18.7,
            });
            let text = format_stats_text(&sensor, stats_24h, stats_30d);
            assert_eq!(
                text,
                "Aare Bern\n\
                 \n\
                 🌡️ *18.3°C 😌 (2 hours ago)*\n\
                 \n\
                 _Over the last 24 hours, the temperature ranged from 17.8°C to 19.2°C, averaging 18.5°C._\n\
                 \n\
                 _Over the last 30 days, the temperature ranged from 14.1°C to 22.4°C, averaging 18.7°C._",
            );
        }

        #[test]
        fn with_caption() {
            let mut sensor = sensor_with_time(6, "Kempraten", Some(18.3), 1);
            sensor.caption = Some("Die Wassertemperatur in Kempraten.".to_string());
            let text = format_stats_text(&sensor, None, None);
            assert!(text.starts_with("Kempraten: _Die Wassertemperatur in Kempraten._\n"));
        }

        #[test]
        fn without_stats() {
            let sensor = make_sensor(42, "Limmat Zürich", None);
            insta::assert_snapshot!(format_stats_text(&sensor, None, None));
        }
    }
}
