//! SQLite persistence for swimming-temperature alerts.
//!
//! [`AlertStore`] is the alert-specific query layer: It borrows the connection pool
//! from the generic [`Database`](crate::db::Database) and exposes the CRUD + state-machine
//! operations the alert commands and the daily scheduler build on. Connection setup and migrations
//! are the [`Database`](crate::db::Database) layer's responsibility.

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqlitePool;
use threema_gateway::ThreemaId;

use crate::{api::SensorId, db::Database};

/// Columns of the `alerts` table, in the order [`Alert`] expects them.
const ALERT_COLUMNS: &str = "uid, threema_id, sensor_id, threshold, status, warm_streak, cold_streak, last_eval_date, created_at";

/// Smallest allowed alert threshold, in °C.
///
/// The inclusive range [`MIN_THRESHOLD`, `MAX_THRESHOLD`] is enforced by the `threshold` `CHECK`
/// constraint in `migrations/0001_create_alerts.sql` and re-used by the bot's command
/// validation. Keep all three in sync.
pub const MIN_THRESHOLD: f64 = 5.0;
/// Largest allowed alert threshold, in °C. See [`MIN_THRESHOLD`].
pub const MAX_THRESHOLD: f64 = 40.0;

/// Lifecycle state of an alert.
///
/// Stored in the `status` TEXT column as the lower-cased variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum AlertStatus {
    /// Waiting for the warm-enough condition; the bot may notify the user.
    Watching,
    /// Already notified; silent until it resets.
    Notified,
}

/// A single alert row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Alert {
    /// Surrogate primary key (aliases SQLite's rowid).
    pub uid: i64,
    // TODO: Use the `threema_gateway::ThreemaId` newtype for this field once the `threema-gateway`
    // crate gains an `sqlx` feature providing `Type`/`Encode`/`Decode` impls. Until then we keep
    // `String` so `Alert` can derive `FromRow` (a foreign newtype can't implement sqlx's
    // traits here due to the orphan rule, which would force a hand-written `FromRow`). The public
    // store methods already take `ThreemaId`, so only this stored representation is affected.
    /// The alerted user's Threema ID (always 8 characters).
    pub threema_id: String,
    /// Gfrörli sensor this alert watches.
    pub sensor_id: SensorId,
    /// Temperature threshold in °C.
    pub threshold: f64,
    /// Alert lifecycle state.
    pub status: AlertStatus,
    /// Consecutive days the afternoon average was at or above the threshold.
    pub warm_streak: u32,
    /// Consecutive days the afternoon average was clearly below the threshold.
    pub cold_streak: u32,
    /// Calendar date (Europe/Zurich local) of the last evaluation, or `None` before the first run.
    pub last_eval_date: Option<NaiveDate>,
    /// Creation time as unix epoch seconds.
    pub created_at: i64,
}

impl Alert {
    /// The creation time as a typed UTC instant.
    pub fn created_at_datetime(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.created_at, 0).unwrap_or_default()
    }
}

/// The alert-specific query layer.
///
/// Cheaply cloneable — it holds a clone of the [`Database`]'s reference-counted [`SqlitePool`] — so
/// it can be shared across the message handler and the background scheduler.
#[derive(Debug, Clone)]
pub struct AlertStore {
    pool: SqlitePool,
}

impl AlertStore {
    /// Create an alert store backed by the given [`Database`]'s connection pool.
    pub fn new(database: &Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }

    /// Insert a new alert, returning the created row.
    ///
    /// Errors if an alert for the same `(threema_id, sensor_id)` already exists, or if the
    /// database `CHECK` constraint rejects the threshold range. (The Threema ID is already validated
    /// by the [`ThreemaId`] type.)
    pub async fn add(
        &self,
        threema_id: ThreemaId,
        sensor_id: SensorId,
        threshold: f64,
    ) -> Result<Alert> {
        let created_at = Utc::now().timestamp();
        let uid = sqlx::query(
            "INSERT INTO alerts (threema_id, sensor_id, threshold, created_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(threema_id.as_str())
        .bind(sensor_id)
        .bind(threshold)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .context("inserting alert")?
        .last_insert_rowid();

        Ok(Alert {
            uid,
            threema_id: threema_id.as_str().to_owned(),
            sensor_id,
            threshold,
            status: AlertStatus::Watching,
            warm_streak: 0,
            cold_streak: 0,
            last_eval_date: None,
            created_at,
        })
    }

    /// Delete an alert, returning whether a row was removed.
    pub async fn remove(&self, threema_id: ThreemaId, sensor_id: SensorId) -> Result<bool> {
        let affected = sqlx::query("DELETE FROM alerts WHERE threema_id = ? AND sensor_id = ?")
            .bind(threema_id.as_str())
            .bind(sensor_id)
            .execute(&self.pool)
            .await
            .context("deleting alert")?
            .rows_affected();
        Ok(affected > 0)
    }

    /// Change an alert's threshold and reset its state machine.
    ///
    /// The streak counters are relative to the old threshold, so a threshold change resets the
    /// alert back to [`AlertStatus::Watching`], with streaks zeroed and
    /// `last_eval_date` cleared.
    pub async fn reset_with_threshold(&self, uid: i64, threshold: f64) -> Result<()> {
        sqlx::query(
            "UPDATE alerts \
             SET threshold = ?, status = ?, warm_streak = 0, cold_streak = 0, last_eval_date = NULL \
             WHERE uid = ?",
        )
        .bind(threshold)
        .bind(AlertStatus::Watching)
        .bind(uid)
        .execute(&self.pool)
        .await
        .context("resetting alert threshold")?;
        Ok(())
    }

    /// Delete all alerts belonging to a user, returning how many were removed.
    pub async fn remove_all(&self, threema_id: ThreemaId) -> Result<u32> {
        let affected = sqlx::query("DELETE FROM alerts WHERE threema_id = ?")
            .bind(threema_id.as_str())
            .execute(&self.pool)
            .await
            .context("deleting all alerts")?
            .rows_affected();
        Ok(u32::try_from(affected).unwrap_or(u32::MAX))
    }

    /// Fetch a single alert by user and sensor.
    pub async fn get(&self, threema_id: ThreemaId, sensor_id: SensorId) -> Result<Option<Alert>> {
        let sql =
            format!("SELECT {ALERT_COLUMNS} FROM alerts WHERE threema_id = ? AND sensor_id = ?");
        sqlx::query_as::<_, Alert>(&sql)
            .bind(threema_id.as_str())
            .bind(sensor_id)
            .fetch_optional(&self.pool)
            .await
            .context("fetching alert")
    }

    /// List all alerts belonging to one user, ordered by sensor.
    pub async fn list_for_user(&self, threema_id: ThreemaId) -> Result<Vec<Alert>> {
        let sql =
            format!("SELECT {ALERT_COLUMNS} FROM alerts WHERE threema_id = ? ORDER BY sensor_id");
        sqlx::query_as::<_, Alert>(&sql)
            .bind(threema_id.as_str())
            .fetch_all(&self.pool)
            .await
            .context("listing user alerts")
    }

    /// List every alert, ordered by `uid` — used by the daily evaluation sweep.
    pub async fn list_active(&self) -> Result<Vec<Alert>> {
        let sql = format!("SELECT {ALERT_COLUMNS} FROM alerts ORDER BY uid");
        sqlx::query_as::<_, Alert>(&sql)
            .fetch_all(&self.pool)
            .await
            .context("listing all alerts")
    }

    /// Total number of alerts.
    pub async fn count(&self) -> Result<u32> {
        let row: (u32,) = sqlx::query_as("SELECT COUNT(*) FROM alerts")
            .fetch_one(&self.pool)
            .await
            .context("counting alerts")?;
        Ok(row.0)
    }

    /// Persist the alert state machine's outcome for one alert after a daily evaluation.
    pub async fn update_state(
        &self,
        uid: i64,
        status: AlertStatus,
        warm_streak: u32,
        cold_streak: u32,
        last_eval_date: NaiveDate,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE alerts \
             SET status = ?, warm_streak = ?, cold_streak = ?, last_eval_date = ? \
             WHERE uid = ?",
        )
        .bind(status)
        .bind(warm_streak)
        .bind(cold_streak)
        .bind(last_eval_date)
        .bind(uid)
        .execute(&self.pool)
        .await
        .context("updating alert state")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory store, backed by an in-memory [`Database`] whose schema persists for the test.
    async fn memory_store() -> AlertStore {
        AlertStore::new(&Database::connect_in_memory().await)
    }

    /// Parse a Threema ID from a literal, panicking on invalid input (test convenience).
    fn tid(id: &str) -> ThreemaId {
        id.parse().unwrap()
    }

    mod connect {
        use super::*;

        #[tokio::test]
        async fn migrations_apply_to_empty_database() {
            let store = memory_store().await;
            assert_eq!(store.count().await.unwrap(), 0);
        }
    }

    mod add {
        use super::*;

        #[tokio::test]
        async fn round_trips_all_fields() {
            let store = memory_store().await;
            let created = store.add(tid("ABCD1234"), SensorId(7), 23.0).await.unwrap();
            assert_eq!(created.threema_id, "ABCD1234");
            assert_eq!(created.sensor_id, SensorId(7));
            assert_eq!(created.threshold, 23.0);
            assert_eq!(created.status, AlertStatus::Watching);
            assert_eq!(created.warm_streak, 0);
            assert_eq!(created.cold_streak, 0);
            assert!(created.last_eval_date.is_none());

            let fetched = store
                .get(tid("ABCD1234"), SensorId(7))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fetched.uid, created.uid);
            assert_eq!(fetched.threema_id, "ABCD1234");
            assert_eq!(fetched.threshold, 23.0);
            assert_eq!(fetched.status, AlertStatus::Watching);

            let for_user = store.list_for_user(tid("ABCD1234")).await.unwrap();
            assert_eq!(for_user.len(), 1);
        }

        #[tokio::test]
        async fn rejects_duplicate_but_allows_other_sensor_or_user() {
            let store = memory_store().await;
            store.add(tid("ABCD1234"), SensorId(7), 23.0).await.unwrap();
            assert!(store.add(tid("ABCD1234"), SensorId(7), 24.0).await.is_err());
            store.add(tid("ABCD1234"), SensorId(8), 23.0).await.unwrap();
            store.add(tid("WXYZ5678"), SensorId(7), 23.0).await.unwrap();
            assert_eq!(store.count().await.unwrap(), 3);
        }

        #[tokio::test]
        async fn rejects_threshold_out_of_range() {
            let store = memory_store().await;
            assert!(
                store
                    .add(tid("ABCD1234"), SensorId(7), MIN_THRESHOLD - 0.1)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .add(tid("ABCD1234"), SensorId(7), MAX_THRESHOLD + 0.1)
                    .await
                    .is_err()
            );
            store
                .add(tid("ABCD1234"), SensorId(7), MIN_THRESHOLD)
                .await
                .unwrap();
            store
                .add(tid("ABCD1234"), SensorId(8), MAX_THRESHOLD)
                .await
                .unwrap();
        }
    }

    mod remove {
        use super::*;

        #[tokio::test]
        async fn deletes_then_reports_absent() {
            let store = memory_store().await;
            store.add(tid("ABCD1234"), SensorId(7), 23.0).await.unwrap();
            assert!(store.remove(tid("ABCD1234"), SensorId(7)).await.unwrap());
            assert!(!store.remove(tid("ABCD1234"), SensorId(7)).await.unwrap());
            assert!(
                store
                    .get(tid("ABCD1234"), SensorId(7))
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn remove_all_only_affects_one_user() {
            let store = memory_store().await;
            store.add(tid("ABCD1234"), SensorId(7), 23.0).await.unwrap();
            store.add(tid("ABCD1234"), SensorId(8), 20.0).await.unwrap();
            store.add(tid("WXYZ5678"), SensorId(7), 23.0).await.unwrap();

            assert_eq!(store.remove_all(tid("ABCD1234")).await.unwrap(), 2);
            assert_eq!(store.remove_all(tid("ABCD1234")).await.unwrap(), 0);
            assert!(
                store
                    .list_for_user(tid("ABCD1234"))
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(store.list_for_user(tid("WXYZ5678")).await.unwrap().len(), 1);
        }
    }

    mod update_state {
        use super::*;

        #[tokio::test]
        async fn persists_status_streaks_and_date() {
            let store = memory_store().await;
            let sub = store.add(tid("ABCD1234"), SensorId(7), 23.0).await.unwrap();
            let date = NaiveDate::from_ymd_opt(2026, 6, 13).unwrap();
            store
                .update_state(sub.uid, AlertStatus::Notified, 0, 2, date)
                .await
                .unwrap();

            let active = store.list_active().await.unwrap();
            assert_eq!(active.len(), 1);
            let updated = &active[0];
            assert_eq!(updated.status, AlertStatus::Notified);
            assert_eq!(updated.warm_streak, 0);
            assert_eq!(updated.cold_streak, 2);
            assert_eq!(updated.last_eval_date, Some(date));
        }
    }

    mod reset_with_threshold {
        use super::*;

        #[tokio::test]
        async fn changes_threshold_and_resets_state() {
            let store = memory_store().await;
            let sub = store.add(tid("ABCD1234"), SensorId(7), 20.0).await.unwrap();
            let date = NaiveDate::from_ymd_opt(2026, 6, 13).unwrap();
            // Drive it into a notified state with non-zero streaks first.
            store
                .update_state(sub.uid, AlertStatus::Notified, 1, 2, date)
                .await
                .unwrap();

            store.reset_with_threshold(sub.uid, 25.0).await.unwrap();

            let updated = store
                .get(tid("ABCD1234"), SensorId(7))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(updated.threshold, 25.0);
            assert_eq!(updated.status, AlertStatus::Watching);
            assert_eq!(updated.warm_streak, 0);
            assert_eq!(updated.cold_streak, 0);
            assert_eq!(updated.last_eval_date, None);
        }
    }
}
