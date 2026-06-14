use std::{collections::HashMap, fmt::Write, sync::Arc};

use async_trait::async_trait;
use chrono::{TimeDelta, TimeZone, Utc};
use threema_gateway::ThreemaId;
use threema_gateway_bot::{
    commands::{CommandStyle, Commands},
    server::handler::{
        Action, CommandType, HandlerError, HandlerResult, MessageContext, MessageHandler, Response,
        TypingHandle,
    },
};

use crate::{
    LOCAL_TIMEZONE,
    api::{
        DailyTemperature, GfroerliClient, HourlyTemperature, Sensor, SensorId, Sponsor,
        format_sponsor_list_text,
    },
    chart::{self, DailyPoint, HourlyPoint},
    store::{AlertStatus, AlertStore, MAX_THRESHOLD, MIN_THRESHOLD},
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
    write!(out, "\n\n🌡️ {}", sensor.format_temperature_reading()).unwrap();
    if let Some(s) = stats_24h {
        write!(
            out,
            "\n\nOver the last 24 hours, the temperature ranged from {:.1}°C to {:.1}°C, averaging {:.1}°C.",
            s.min, s.max, s.avg
        )
        .unwrap();
    }
    if let Some(s) = stats_30d {
        write!(
            out,
            "\n\nOver the last 30 days, the temperature ranged from {:.1}°C to {:.1}°C, averaging {:.1}°C.",
            s.min, s.max, s.avg
        )
        .unwrap();
    }
    if let Some(max) = sensor.maximum_temperature {
        write!(
            out,
            "\n\nThe highest temperature ever measured at this location was {max:.1}°C."
        )
        .unwrap();
    }
    out
}

/// Wrap each paragraph of a description in Threema italic markers. Threema
/// markup is scoped to a single paragraph, so blocks separated by blank lines
/// each need their own `_..._` pair. A trailing space before the closing `_`
/// prevents the marker from fusing with any URL at the end of a paragraph.
fn italicize_paragraphs(text: &str) -> String {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!("_{p} _"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Format the `/sponsor <sensor>` response text.
fn format_sponsor_text(sensor: &Sensor, sponsor: &Sponsor) -> String {
    let preposition = sponsor.sponsor_type.preposition();
    let description = sponsor
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    match description {
        Some(desc) => format!(
            "The sensor \"{}\" is {preposition} *{}*:\n\n{}",
            sensor.device_name,
            sponsor.name,
            italicize_paragraphs(desc)
        ),
        None => format!(
            "The sensor \"{}\" is {preposition} *{}*.",
            sensor.device_name, sponsor.name
        ),
    }
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

/// Maximum number of alerts a single user may hold.
const MAX_ALERTS_PER_USER: usize = 10;

/// Usage hint shown for malformed `/alert` invocations.
const ALERT_USAGE: &str = "Get notified once a sensor's water body warms up.\n\n\
     A message will be sent to you when the afternoon average temperature is above \
     your chosen temperature for 2 days in a row.\n\n\
     Usage: /alert <sensor> <temperature>\n\
     Example: /alert Aare 20\n\n\
     List your alerts with /alerts, remove one with /unalert <sensor>.";

/// Usage hint shown for malformed `/unalert` invocations.
const UNALERT_USAGE: &str = "Remove an alert.\n\n\
     Usage: /unalert <sensor>\n\
     Example: /unalert Aare\n\n\
     Remove all your alerts with /unalert all.";

/// One row of the `/alerts` listing.
struct AlertEntry {
    sensor_name: String,
    threshold: f64,
    status: AlertStatus,
}

/// Parse and range-check a threshold argument, returning a user-facing error message on failure.
fn parse_threshold(raw: &str) -> Result<f64, String> {
    let value: f64 = raw
        .parse()
        .map_err(|_| format!("\"{raw}\" is not a valid temperature.\n\nExample: /alert Aare 20"))?;
    if !(MIN_THRESHOLD..=MAX_THRESHOLD).contains(&value) {
        return Err(format!(
            "The temperature must be between {MIN_THRESHOLD:.0}°C and {MAX_THRESHOLD:.0}°C."
        ));
    }
    Ok(value)
}

/// Confirmation shown after a new alert is created.
fn format_alert_added(sensor_name: &str, threshold: f64) -> String {
    format!(
        "🔔 Alert set for *{sensor_name}*. I'll message you once the average afternoon water \
         temperature reaches *{threshold:.1}°C* two days in a row.\n\n\
         See your alerts with /alerts, or remove this one with /unalert {sensor_name}.\n\n\
         _To do this I store your Threema ID; removing the alert deletes it again._"
    )
}

/// Reply shown when an existing alert's threshold is changed.
fn format_alert_updated(sensor_name: &str, threshold: f64) -> String {
    format!(
        "🔔 Updated your alert for *{sensor_name}* to *{threshold:.1}°C*. I'll message you once the \
         afternoon water temperature reaches it two days in a row."
    )
}

/// Reply shown when the user re-sends an alert they already have at the same threshold.
fn format_alert_unchanged(sensor_name: &str, threshold: f64) -> String {
    format!(
        "You already have an alert for *{sensor_name}* at {threshold:.1}°C. Send a different \
         temperature to change it, or /unalert {sensor_name} to remove it."
    )
}

/// Reply shown when the per-user alert cap is reached.
fn format_alert_limit_reached() -> String {
    format!(
        "You already have the maximum of {MAX_ALERTS_PER_USER} alerts. Remove one with \
         /unalert <sensor> before adding another."
    )
}

/// Confirmation shown after an alert is removed.
fn format_alert_removed(sensor_name: &str) -> String {
    format!("🔕 Removed your alert for *{sensor_name}*.")
}

/// Reply shown when removing an alert the user doesn't have.
fn format_no_alert_for(sensor_name: &str) -> String {
    format!("You don't have an alert for *{sensor_name}*.")
}

/// Reply shown after `/unalert all`.
fn format_alerts_cleared(count: u32) -> String {
    match count {
        0 => "You have no alerts to remove.".to_string(),
        1 => "🔕 Removed your alert.".to_string(),
        n => format!("🔕 Removed all {n} of your alerts."),
    }
}

/// Render the `/alerts` listing.
fn format_alerts_list(entries: &[AlertEntry]) -> String {
    if entries.is_empty() {
        return "You have no alerts yet.\n\n\
                Set one with /alert <sensor> <temperature>, e.g. /alert Aare 20."
            .to_string();
    }

    let mut out = String::from("Your alerts:\n\n");
    for entry in entries {
        let state = match entry.status {
            AlertStatus::Watching => "watching",
            AlertStatus::Notified => "already notified",
        };
        writeln!(
            out,
            "- *{}* at {:.1}°C (_{state}_)",
            entry.sensor_name, entry.threshold
        )
        .unwrap();
    }
    out.truncate(out.trim_end().len());
    out
}

/// Convert API daily aggregates into chart points, anchored at noon local time.
///
/// The aggregates are already in [`LOCAL_TIMEZONE`] (see
/// [`DailyTemperature::aggregation_date`](crate::api::DailyTemperature::aggregation_date)), so the
/// point's timestamp is built directly in that zone rather than converted.
fn daily_points(daily: &[DailyTemperature]) -> Vec<DailyPoint> {
    daily
        .iter()
        .filter_map(|d| {
            let naive = d.aggregation_date.and_hms_opt(12, 0, 0)?;
            Some(DailyPoint {
                x: LOCAL_TIMEZONE.from_local_datetime(&naive).single()?,
                min: d.minimum_temperature,
                max: d.maximum_temperature,
                avg: d.average_temperature,
            })
        })
        .collect()
}

/// Convert API hourly aggregates into chart points, keeping only those within the last 24 hours.
///
/// The aggregates are already in [`LOCAL_TIMEZONE`] (see
/// [`HourlyTemperature::aggregation_hour`](crate::api::HourlyTemperature::aggregation_hour)), so the
/// point's timestamp is built directly in that zone, and the 24-hour cutoff is taken in local time.
fn hourly_points(hourly: &[HourlyTemperature]) -> Vec<HourlyPoint> {
    let cutoff = Utc::now().with_timezone(&LOCAL_TIMEZONE) - TimeDelta::hours(24);
    let mut points: Vec<HourlyPoint> = hourly
        .iter()
        .filter_map(|h| {
            let naive = h
                .aggregation_date
                .and_hms_opt(u32::from(h.aggregation_hour), 0, 0)?;
            let x = LOCAL_TIMEZONE.from_local_datetime(&naive).single()?;
            (x >= cutoff).then_some(HourlyPoint {
                x,
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
    alert_store: AlertStore,
    maintainer_ids: Vec<ThreemaId>,
}

impl GfroerliHandler {
    pub fn new(
        client: Arc<GfroerliClient>,
        alert_store: AlertStore,
        maintainer_ids: Vec<ThreemaId>,
    ) -> Self {
        Self {
            client,
            alert_store,
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

        // Fetch sensor details (for the all-time maximum), daily (last 30
        // days) and hourly (yesterday + today for the last 24h window)
        // aggregates in parallel
        let today = Utc::now().date_naive();
        let daily_from = today - TimeDelta::days(30);
        let hourly_from = today - TimeDelta::days(1);
        let details_fut = self.client.sensor_details(sensor.id);
        let daily_fut = self
            .client
            .daily_temperatures(sensor.id, daily_from, today, 30);
        let hourly_fut = self
            .client
            .hourly_temperatures(sensor.id, hourly_from, today, 48);
        let (sensor, daily, hourly) =
            tokio::try_join!(details_fut, daily_fut, hourly_fut).map_err(HandlerError::from)?;

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
        let rendered_at = Utc::now().with_timezone(&LOCAL_TIMEZONE);
        let png = chart::render_sensor_charts(
            &sensor.device_name,
            rendered_at,
            &hourly_chart,
            &daily_chart,
        )
        .map_err(HandlerError::from)?;

        Ok(Action::Respond(vec![Response::image(
            png,
            "image/png",
            Some(text),
        )]))
    }

    /// Handle `/sponsors`: list all project sponsors and how many sensors
    /// each one backs.
    async fn handle_sponsors(&self, typing: &TypingHandle) -> HandlerResult<Action> {
        typing.send();

        let sponsors = self.client.sponsors().await.map_err(HandlerError::from)?;
        let text = format_sponsor_list_text(sponsors);
        Ok(Action::Respond(vec![Response::text(text)]))
    }

    /// Handle `/sponsor <query>`: look up a sensor by name or ID and show
    /// which sponsor backs it.
    async fn handle_sponsor(&self, args: &str, typing: &TypingHandle) -> HandlerResult<Action> {
        // Validate query
        let query = args.trim();
        if query.is_empty() {
            return Ok(Action::Respond(vec![Response::text(
                "Please specify a sensor name or ID.\n\nExample: /sponsor Aare\nExample: /sponsor 1\n\nUse /sensors to list all available sensors.",
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
        let sensor = match resolve_single_sensor(query, matches, "/sponsor 1") {
            Ok(sensor) => sensor,
            Err(msg) => return Ok(Action::Respond(vec![Response::text(msg)])),
        };

        // Fetch sponsor for this sensor
        let sponsor = self
            .client
            .sensor_sponsor(sensor.id)
            .await
            .map_err(HandlerError::from)?;

        let text = match sponsor {
            Some(sponsor) => format_sponsor_text(&sensor, &sponsor),
            None => format!(
                "No sponsor information available for \"{}\".",
                sensor.device_name
            ),
        };
        Ok(Action::Respond(vec![Response::text(text)]))
    }

    /// Handle `/alert <sensor> <temp>`: create a new alert. (Removal lives in `/unalert`.)
    async fn handle_alert(
        &self,
        ctx: &MessageContext,
        args: &str,
        typing: &TypingHandle,
    ) -> HandlerResult<Action> {
        // Handle missing args
        let args = args.trim();
        if args.is_empty() {
            return Ok(Action::Respond(vec![Response::text(ALERT_USAGE)]));
        }

        // Add form: `<sensor> <temperature>`, where the temperature is the final token. The
        // threshold is mandatory, a bare `/alert <sensor>` falls through to the usage hint.
        let Some((query, temp_raw)) = args.rsplit_once(char::is_whitespace) else {
            return Ok(Action::Respond(vec![Response::text(ALERT_USAGE)]));
        };
        let query = query.trim();
        if query.is_empty() {
            return Ok(Action::Respond(vec![Response::text(ALERT_USAGE)]));
        }

        // Parse threshold value
        let threshold = match parse_threshold(temp_raw.trim()) {
            Ok(threshold) => threshold,
            Err(msg) => return Ok(Action::Respond(vec![Response::text(msg)])),
        };

        // Args could be parsed, show typing indicator
        typing.send();

        // Resolve the sensor
        let matches = self
            .client
            .find_sensors(query)
            .await
            .map_err(HandlerError::from)?;
        let sensor = match resolve_single_sensor(query, matches, "/alert 1 20") {
            Ok(sensor) => sensor,
            Err(msg) => return Ok(Action::Respond(vec![Response::text(msg)])),
        };

        // Get existing alerts
        let existing = self
            .alert_store
            .list_for_user(ctx.sender_identity)
            .await
            .map_err(HandlerError::from)?;

        // Check if sensor alert exists already
        if let Some(sub) = existing.iter().find(|s| s.sensor_id == sensor.id) {
            // Alert found without change in threshold - no-op
            if (sub.threshold - threshold).abs() < 0.05 {
                return Ok(Action::Respond(vec![Response::text(
                    format_alert_unchanged(&sensor.device_name, sub.threshold),
                )]));
            }

            // Changed threshold - update and reset state
            self.alert_store
                .reset_with_threshold(sub.uid, threshold)
                .await
                .map_err(HandlerError::from)?;
            return Ok(Action::Respond(vec![Response::text(format_alert_updated(
                &sensor.device_name,
                threshold,
            ))]));
        }

        // Check alert limit
        if existing.len() >= MAX_ALERTS_PER_USER {
            return Ok(Action::Respond(vec![Response::text(
                format_alert_limit_reached(),
            )]));
        }

        // Store new alert
        self.alert_store
            .add(ctx.sender_identity, sensor.id, threshold)
            .await
            .map_err(HandlerError::from)?;
        Ok(Action::Respond(vec![Response::text(format_alert_added(
            &sensor.device_name,
            threshold,
        ))]))
    }

    /// Handle `/unalert <sensor>` (remove one) and `/unalert all` (remove every alert).
    async fn handle_unalert(
        &self,
        ctx: &MessageContext,
        args: &str,
        typing: &TypingHandle,
    ) -> HandlerResult<Action> {
        // Handle missing args
        let query = args.trim();
        if query.is_empty() {
            return Ok(Action::Respond(vec![Response::text(UNALERT_USAGE)]));
        }

        // `/unalert all`: Clear every alert for this user.
        if query.eq_ignore_ascii_case("all") {
            typing.send();
            let removed = self
                .alert_store
                .remove_all(ctx.sender_identity)
                .await
                .map_err(HandlerError::from)?;
            return Ok(Action::Respond(vec![Response::text(
                format_alerts_cleared(removed),
            )]));
        }

        typing.send();

        // Resolve the sensor
        let matches = self
            .client
            .find_sensors(query)
            .await
            .map_err(HandlerError::from)?;
        let sensor = match resolve_single_sensor(query, matches, "/unalert 1") {
            Ok(sensor) => sensor,
            Err(msg) => return Ok(Action::Respond(vec![Response::text(msg)])),
        };

        // Remove alert
        let removed = self
            .alert_store
            .remove(ctx.sender_identity, sensor.id)
            .await
            .map_err(HandlerError::from)?;
        let text = if removed {
            format_alert_removed(&sensor.device_name)
        } else {
            format_no_alert_for(&sensor.device_name)
        };

        Ok(Action::Respond(vec![Response::text(text)]))
    }

    /// Handle `/alerts`: list the user's active alerts.
    async fn handle_alerts(
        &self,
        ctx: &MessageContext,
        typing: &TypingHandle,
    ) -> HandlerResult<Action> {
        typing.send();

        // Get user's alerts
        let alerts = self
            .alert_store
            .list_for_user(ctx.sender_identity)
            .await
            .map_err(HandlerError::from)?;
        if alerts.is_empty() {
            return Ok(Action::Respond(vec![Response::text(format_alerts_list(
                &[],
            ))]));
        }

        // Resolve sensor ids to names via the (cached) sensor list.
        let names: HashMap<SensorId, String> = self
            .client
            .sensors()
            .await
            .map_err(HandlerError::from)?
            .into_iter()
            .map(|sensor| (sensor.id, sensor.device_name))
            .collect();
        let entries: Vec<AlertEntry> = alerts
            .into_iter()
            .map(|sub| AlertEntry {
                sensor_name: names
                    .get(&sub.sensor_id)
                    .cloned()
                    .unwrap_or_else(|| format!("#{}", sub.sensor_id)),
                threshold: sub.threshold,
                status: sub.status,
            })
            .collect();
        Ok(Action::Respond(vec![Response::text(format_alerts_list(
            &entries,
        ))]))
    }

    /// Handle `/about`: Show information about the Gfrörli project.
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
        Some("🥶🤖 *Gfrörli Bot:* Check water temperatures in Swiss water bodies.")
    }

    fn commands() -> Commands {
        Commands::new()
            .style(CommandStyle::Slash)
            .group("sensor", "Sensor Commands", |group| {
                group
                    .register("sensors", "List all available sensors")
                    .register("temp", "Get current temperature (e.g. /temp Aare)")
                    .register(
                        "stats",
                        "Show stats and charts for a sensor (e.g. /stats Aare)",
                    )
            })
            .group("sponsor", "Sponsor Commands", |group| {
                group
                    .register("sponsors", "List all project sponsors")
                    .register("sponsor", "Show sponsor for a sensor (e.g. /sponsor Aare)")
            })
            .group("alert", "Alert Commands", |group| {
                group
                    .register(
                        "alert",
                        "Get notified when a water body warms up (e.g. /alert Aare 20)",
                    )
                    .register("alerts", "List your active alerts")
                    .register(
                        "unalert",
                        "Remove an alert (e.g. /unalert Aare, or /unalert all)",
                    )
            })
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
        ctx: &MessageContext,
        command: &str,
        args: &str,
        _command_type: CommandType,
        typing: &TypingHandle,
    ) -> HandlerResult<Action> {
        match command {
            "sensors" => self.handle_sensors(typing).await,
            "temp" => self.handle_temp(args, typing).await,
            "stats" => self.handle_stats(args, typing).await,
            "sponsors" => self.handle_sponsors(typing).await,
            "sponsor" => self.handle_sponsor(args, typing).await,
            "alert" => self.handle_alert(ctx, args, typing).await,
            "alerts" => self.handle_alerts(ctx, typing).await,
            "unalert" => self.handle_unalert(ctx, args, typing).await,
            "about" => Ok(self.handle_about()),
            _ => Ok(Action::ShowHelp { prelude: None }),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::{SensorId, SponsorId, SponsorType};

    use super::*;

    mod parse_threshold {
        use rstest::rstest;

        use super::*;

        #[rstest]
        #[case("20", 20.0)]
        #[case("23.5", 23.5)]
        #[case("5", MIN_THRESHOLD)]
        #[case("40", MAX_THRESHOLD)]
        fn accepts_valid(#[case] input: &str, #[case] expected: f64) {
            assert_eq!(parse_threshold(input), Ok(expected));
        }

        #[rstest]
        #[case("4.9")]
        #[case("40.1")]
        #[case("hot")]
        #[case("")]
        fn rejects_invalid(#[case] input: &str) {
            assert!(parse_threshold(input).is_err());
        }
    }

    mod format_alert {
        use super::*;

        #[test]
        fn added() {
            insta::assert_snapshot!(format_alert_added("Aare", 20.0));
        }

        #[test]
        fn updated() {
            insta::assert_snapshot!(format_alert_updated("Aare", 25.0));
        }

        #[test]
        fn unchanged() {
            insta::assert_snapshot!(format_alert_unchanged("Aare", 20.0));
        }

        #[test]
        fn limit_reached() {
            insta::assert_snapshot!(format_alert_limit_reached());
        }

        #[test]
        fn removed() {
            insta::assert_snapshot!(format_alert_removed("Aare"));
        }

        #[test]
        fn no_alert_for() {
            insta::assert_snapshot!(format_no_alert_for("Aare"));
        }

        #[test]
        fn cleared_none() {
            insta::assert_snapshot!(format_alerts_cleared(0));
        }

        #[test]
        fn cleared_one() {
            insta::assert_snapshot!(format_alerts_cleared(1));
        }

        #[test]
        fn cleared_many() {
            insta::assert_snapshot!(format_alerts_cleared(3));
        }
    }

    mod format_alerts_list {
        use super::*;

        #[test]
        fn empty() {
            insta::assert_snapshot!(format_alerts_list(&[]));
        }

        #[test]
        fn with_entries() {
            let entries = [
                AlertEntry {
                    sensor_name: "Aare".to_string(),
                    threshold: 20.0,
                    status: AlertStatus::Watching,
                },
                AlertEntry {
                    sensor_name: "Aare Bern".to_string(),
                    threshold: 18.5,
                    status: AlertStatus::Notified,
                },
            ];
            insta::assert_snapshot!(format_alerts_list(&entries));
        }
    }

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
            id: SensorId(id),
            device_name: name.to_string(),
            caption: None,
            latest_temperature: temp,
            latest_measurement_at: None,
            maximum_temperature: None,
        }
    }

    fn make_sponsor(name: &str, description: Option<&str>, sponsor_type: SponsorType) -> Sponsor {
        Sponsor {
            id: SponsorId(1),
            name: name.to_string(),
            description: description.map(str::to_string),
            sponsor_type,
            created_at: None,
            sensor_ids: Vec::new(),
        }
    }

    mod format_sponsor_text {
        use super::*;

        #[test]
        fn sponsored_by_with_description() {
            let sensor = make_sensor(1, "Aare Bern", None);
            let sponsor = make_sponsor(
                "Threema",
                Some("Secure messaging from Switzerland"),
                SponsorType::Sponsor,
            );
            insta::assert_snapshot!(format_sponsor_text(&sensor, &sponsor));
        }

        #[test]
        fn powered_by_partner() {
            let sensor = make_sensor(2, "Rhein Basel", None);
            let sponsor = make_sponsor("Partner Ltd", Some("A data partner"), SponsorType::Partner);
            insta::assert_snapshot!(format_sponsor_text(&sensor, &sponsor));
        }

        #[test]
        fn powered_by_public_data_provider() {
            let sensor = make_sensor(3, "Limmat Zürich", None);
            let sponsor = make_sponsor(
                "MeteoSwiss",
                Some("Swiss weather service"),
                SponsorType::PublicDataProvider,
            );
            insta::assert_snapshot!(format_sponsor_text(&sensor, &sponsor));
        }

        #[test]
        fn no_description() {
            let sensor = make_sensor(1, "Aare Bern", None);
            let sponsor = make_sponsor("Threema", None, SponsorType::Sponsor);
            insta::assert_snapshot!(format_sponsor_text(&sensor, &sponsor));
        }

        #[test]
        fn multi_paragraph_description() {
            let sensor = make_sensor(1, "Zugersee", None);
            let sponsor = make_sponsor(
                "Segel Club Cham",
                Some("First paragraph about the club.\n\nMehr Infos unter https://www.scc.ch/"),
                SponsorType::Sponsor,
            );
            insta::assert_snapshot!(format_sponsor_text(&sensor, &sponsor));
        }

        #[test]
        fn empty_description_treated_as_none() {
            let sensor = make_sensor(1, "Aare Bern", None);
            let sponsor = make_sponsor("Threema", Some("   "), SponsorType::Sponsor);
            insta::assert_snapshot!(format_sponsor_text(&sensor, &sponsor));
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
            assert_eq!(sensor.id, SensorId(1));
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
                id: SensorId(id),
                device_name: name.to_string(),
                caption: None,
                latest_temperature: temp,
                latest_measurement_at: Some(Utc::now() - TimeDelta::hours(hours_ago)),
                maximum_temperature: None,
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
                 🌡️ *18.3°C* 😌 (_2 hours ago_)\n\
                 \n\
                 Over the last 24 hours, the temperature ranged from 17.8°C to 19.2°C, averaging 18.5°C.\n\
                 \n\
                 Over the last 30 days, the temperature ranged from 14.1°C to 22.4°C, averaging 18.7°C.",
            );
        }

        #[test]
        fn with_all_time_maximum() {
            let mut sensor = sensor_with_time(1, "Aare Bern", Some(18.3), 1);
            sensor.maximum_temperature = Some(27.4);
            let text = format_stats_text(&sensor, None, None);
            assert!(text.ends_with(
                "\n\nThe highest temperature ever measured at this location was 27.4°C."
            ));
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
