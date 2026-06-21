//! Background scheduler that evaluates alerts once per day and sends notifications.
//!
//! [`run`] loops forever, waking every [`POLL_INTERVAL`]. After [`EVAL_HOUR`] local time (once the
//! swim window has closed) it evaluates every alert not yet evaluated today, persists the new state,
//! and sends a Threema message when one fires. The `last_eval_date` column is the once-per-day,
//! restart-safe guard; the pure decision logic lives in [`crate::alert`].

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::{NaiveDate, Timelike, Utc};
use threema_gateway::ThreemaId;
use threema_gateway_bot::client::ThreemaClient;
use tracing::{debug, info, trace, warn};

use crate::{
    LOCAL_TIMEZONE,
    alert::{self, AlertState, Outcome},
    api::{GfroerliClient, SensorId},
    store::{Alert, AlertStore, NotificationReason},
};

/// How often the scheduler wakes to check whether evaluation is due.
const POLL_INTERVAL: Duration = Duration::from_mins(5);
/// Local hour (Europe/Zurich) at or after which alerts are evaluated: The early evening, once the
/// swim window (12–18h) has closed.
const EVAL_HOUR: u32 = 19;
/// Upper bound on hourly buckets fetched per sensor per day (24 is enough; leave headroom).
const HOURLY_LIMIT: u32 = 48;

/// Run the daily alert scheduler forever. Intended to be `tokio::spawn`ed alongside the bot server.
pub async fn run(store: AlertStore, client: Arc<GfroerliClient>, threema: Arc<ThreemaClient>) {
    info!(
        "Alert scheduler started: polling every {POLL_INTERVAL:?}, evaluating after {EVAL_HOUR}:00 local time"
    );
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    loop {
        ticker.tick().await;
        if let Err(err) = evaluate_due_alerts(&store, &client, &threema).await {
            warn!("Alert scheduler tick failed: {err:#}");
        }
    }
}

/// Whether an alert still needs evaluating today (never evaluated, or last evaluated on an earlier
/// day).
fn due_today(alert: &Alert, today: NaiveDate) -> bool {
    alert.last_eval_date != Some(today)
}

/// One evaluation pass: evaluate every alert due today and notify on those that fire.
async fn evaluate_due_alerts(
    store: &AlertStore,
    client: &GfroerliClient,
    threema: &ThreemaClient,
) -> Result<()> {
    let now = Utc::now().with_timezone(&LOCAL_TIMEZONE);
    if now.hour() < EVAL_HOUR {
        // Too early
        trace!("Not evaluating alerts, too early");
        return Ok(());
    }
    let today = now.date_naive();

    // Find due alerts
    let due_alerts: Vec<Alert> = store
        .list_active()
        .await
        .context("listing active alerts")?
        .into_iter()
        .filter(|alert| due_today(alert, today))
        .collect();
    if due_alerts.is_empty() {
        trace!("No alerts due for evaluation");
        return Ok(());
    }
    let due_count = due_alerts.len();

    // Resolve sensor names once (cached) for the notification text.
    let names: HashMap<SensorId, String> = client
        .sensors()
        .await
        .context("fetching sensor list")?
        .into_iter()
        .map(|sensor| (sensor.id, sensor.device_name))
        .collect();

    // Group alerts by sensor so each sensor's data is fetched at most once.
    let mut by_sensor: HashMap<SensorId, Vec<Alert>> = HashMap::new();
    for alert in due_alerts {
        by_sensor.entry(alert.sensor_id).or_default().push(alert);
    }
    info!(
        "Evaluating {due_count} due alert(s) across {} sensor(s) for {today}",
        by_sensor.len()
    );

    // Process all due alerts
    for (sensor_id, alerts) in by_sensor {
        // Fetch today's hourly temperatures
        let hourly = match client
            .hourly_temperatures(sensor_id, today, today, HOURLY_LIMIT)
            .await
        {
            Ok(hourly) => hourly,
            Err(err) => {
                warn!("Sensor {sensor_id}: hourly fetch failed, skipping: {err:#}");
                continue;
            }
        };

        // Calculate swim-window average
        let Some(swim_avg) = alert::swim_window_average(&hourly, today) else {
            debug!("Sensor {sensor_id}: no swim-window data for {today}, leaving alerts for later");
            continue;
        };

        // Look up cached sensor name and process alerts
        let sensor_name = names
            .get(&sensor_id)
            .cloned()
            .unwrap_or_else(|| format!("#{sensor_id}"));
        debug!(
            "Sensor {sensor_id} ({sensor_name}): swim-window average {swim_avg:.1}°C, evaluating {} alert(s)",
            alerts.len()
        );
        for alert in &alerts {
            process_alert(store, threema, alert, swim_avg, &sensor_name, today).await;
        }
    }

    Ok(())
}

/// Evaluate one alert against the day's swim-window average and act on the outcome.
async fn process_alert(
    store: &AlertStore,
    threema: &ThreemaClient,
    alert: &Alert,
    swim_avg: f64,
    sensor_name: &str,
    today: NaiveDate,
) {
    let state = AlertState {
        status: alert.status,
        warm_streak: alert.warm_streak,
        cold_streak: alert.cold_streak,
    };
    let transition = alert::evaluate(state, Some(swim_avg), alert.threshold);

    match transition.outcome {
        Outcome::Notify => {
            // Send first, persist second: if the send fails we leave the state untouched so the next
            // tick re-evaluates to `Notify` and retries, rather than silently marking it done.

            let Ok(to) = alert.threema_id.parse::<ThreemaId>() else {
                warn!(
                    "Alert {}: stored Threema ID {:?} is invalid, skipping",
                    alert.uid, alert.threema_id
                );
                return;
            };
            let message = format_notification(sensor_name, swim_avg, alert.threshold);
            match threema.send_text(&to, &message).await {
                Ok(_) => {
                    info!(
                        "Alert {}: fired, notified {} that {sensor_name} reached {swim_avg:.1}°C (threshold {:.1}°C)",
                        alert.uid, alert.threema_id, alert.threshold
                    );
                    record_notification(
                        store,
                        alert,
                        NotificationReason::ThresholdReached,
                        swim_avg,
                    )
                    .await;
                    persist(store, alert.uid, &transition.state, today).await;
                }
                Err(err) => warn!(
                    "Alert {}: notification send failed, will retry: {err}",
                    alert.uid
                ),
            }
        }
        Outcome::Reset => {
            info!(
                "Alert {}: reset to watching, {sensor_name} cooled to {swim_avg:.1}°C (threshold {:.1}°C)",
                alert.uid, alert.threshold
            );
            persist(store, alert.uid, &transition.state, today).await;
        }
        Outcome::Quiet => {
            debug!(
                "Alert {}: no change for {sensor_name} (status {:?}, warm streak {}, cold streak {})",
                alert.uid,
                transition.state.status,
                transition.state.warm_streak,
                transition.state.cold_streak
            );
            persist(store, alert.uid, &transition.state, today).await;
        }
    }
}

/// Append an audit-log entry for a sent notification, logging (but not propagating) a write
/// failure.
async fn record_notification(
    store: &AlertStore,
    alert: &Alert,
    reason: NotificationReason,
    swim_avg: f64,
) {
    if let Err(err) = store
        .log_notification(
            alert.uid,
            alert.sensor_id,
            reason,
            swim_avg,
            alert.threshold,
        )
        .await
    {
        warn!(
            "Alert {}: failed to write audit log entry: {err:#}",
            alert.uid
        );
    }
}

/// Persist an alert's post-evaluation state, logging (but not propagating) a write failure.
async fn persist(store: &AlertStore, uid: i64, state: &AlertState, today: NaiveDate) {
    if let Err(err) = store
        .update_state(
            uid,
            state.status,
            state.warm_streak,
            state.cold_streak,
            today,
        )
        .await
    {
        warn!("Alert {uid}: failed to persist evaluation state: {err:#}");
    }
}

/// The notification message sent when an alert fires.
fn format_notification(sensor_name: &str, swim_avg: f64, threshold: f64) -> String {
    format!(
        "🏊 *{sensor_name}* is warm enough for a swim! The afternoon water temperature reached \
         *{swim_avg:.1}°C* today, at or above your {threshold:.1}°C alert.\n\n\
         Check the latest reading anytime with: /temp {sensor_name}\n\n\
         Or view detailed statistics with: /stats {sensor_name}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::AlertStatus;

    fn make_alert(last_eval_date: Option<NaiveDate>) -> Alert {
        Alert {
            uid: 1,
            threema_id: "ABCD1234".to_owned(),
            sensor_id: SensorId(1),
            threshold: 20.0,
            status: AlertStatus::Watching,
            warm_streak: 0,
            cold_streak: 0,
            last_eval_date,
            created_at: 0,
        }
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    mod due_today {
        use super::*;

        #[test]
        fn never_evaluated_is_due() {
            assert!(due_today(&make_alert(None), date(2026, 6, 14)));
        }

        #[test]
        fn evaluated_earlier_is_due() {
            let alert = make_alert(Some(date(2026, 6, 13)));
            assert!(due_today(&alert, date(2026, 6, 14)));
        }

        #[test]
        fn evaluated_today_is_not_due() {
            let today = date(2026, 6, 14);
            assert!(!due_today(&make_alert(Some(today)), today));
        }
    }

    mod format_notification {
        use super::*;

        #[test]
        fn message() {
            insta::assert_snapshot!(format_notification("Aare", 20.3, 20.0));
        }
    }
}
