//! SQLite persistence for swimming-temperature alert subscriptions.
//!
//! [`SubscriptionStore`] is the subscription-specific query layer: It borrows the connection pool
//! from the generic [`Database`](crate::db::Database) and exposes the CRUD + state-machine
//! operations the alert commands and the daily scheduler build on. Connection setup and migrations
//! are the [`Database`](crate::db::Database) layer's responsibility.

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqlitePool;
use threema_gateway::ThreemaId;

use crate::{api::SensorId, db::Database};

/// Columns of the `subscriptions` table, in the order [`Subscription`] expects them.
const SUBSCRIPTION_COLUMNS: &str = "uid, threema_id, sensor_id, threshold, status, warm_streak, cold_streak, last_eval_date, created_at";

/// Smallest allowed alert threshold, in °C.
///
/// The inclusive range [`MIN_THRESHOLD`, `MAX_THRESHOLD`] is enforced by the `threshold` `CHECK`
/// constraint in `migrations/0001_create_subscriptions.sql` and re-used by the bot's command
/// validation. Keep all three in sync.
pub const MIN_THRESHOLD: f64 = 5.0;
/// Largest allowed alert threshold, in °C. See [`MIN_THRESHOLD`].
pub const MAX_THRESHOLD: f64 = 40.0;

/// Lifecycle state of a subscription's alert.
///
/// Stored in the `status` TEXT column as the lower-cased variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum SubscriptionStatus {
    /// Waiting for the warm-enough condition; the bot may notify the user.
    Watching,
    /// Already notified; silent until it resets.
    Notified,
}

/// A single alert subscription row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Subscription {
    /// Surrogate primary key (aliases SQLite's rowid).
    pub uid: i64,
    // TODO: Use the `threema_gateway::ThreemaId` newtype for this field once the `threema-gateway`
    // crate gains an `sqlx` feature providing `Type`/`Encode`/`Decode` impls. Until then we keep
    // `String` so `Subscription` can derive `FromRow` (a foreign newtype can't implement sqlx's
    // traits here due to the orphan rule, which would force a hand-written `FromRow`). The public
    // store methods already take `ThreemaId`, so only this stored representation is affected.
    /// Subscriber's Threema ID (always 8 characters).
    pub threema_id: String,
    /// Gfrörli sensor this subscription watches.
    pub sensor_id: SensorId,
    /// Temperature threshold in °C.
    pub threshold: f64,
    /// Alert lifecycle state.
    pub status: SubscriptionStatus,
    /// Consecutive days the afternoon average was at or above the threshold.
    pub warm_streak: u32,
    /// Consecutive days the afternoon average was clearly below the threshold.
    pub cold_streak: u32,
    /// Calendar date (Europe/Zurich local) of the last evaluation, or `None` before the first run.
    pub last_eval_date: Option<NaiveDate>,
    /// Creation time as unix epoch seconds.
    pub created_at: i64,
}

impl Subscription {
    /// The creation time as a typed UTC instant.
    pub fn created_at_datetime(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.created_at, 0).unwrap_or_default()
    }
}

/// The subscription-specific query layer.
///
/// Cheaply cloneable — it holds a clone of the [`Database`]'s reference-counted [`SqlitePool`] — so
/// it can be shared across the message handler and the background scheduler.
#[derive(Debug, Clone)]
pub struct SubscriptionStore {
    pool: SqlitePool,
}

impl SubscriptionStore {
    /// Create a subscription store backed by the given [`Database`]'s connection pool.
    pub fn new(database: &Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }

    /// Insert a new subscription, returning the created row.
    ///
    /// Errors if a subscription for the same `(threema_id, sensor_id)` already exists, or if the
    /// database `CHECK` constraint rejects the threshold range. (The Threema ID is already validated
    /// by the [`ThreemaId`] type.)
    pub async fn add(
        &self,
        threema_id: ThreemaId,
        sensor_id: SensorId,
        threshold: f64,
    ) -> Result<Subscription> {
        let created_at = Utc::now().timestamp();
        let uid = sqlx::query(
            "INSERT INTO subscriptions (threema_id, sensor_id, threshold, created_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(threema_id.as_str())
        .bind(sensor_id)
        .bind(threshold)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .context("inserting subscription")?
        .last_insert_rowid();

        Ok(Subscription {
            uid,
            threema_id: threema_id.as_str().to_owned(),
            sensor_id,
            threshold,
            status: SubscriptionStatus::Watching,
            warm_streak: 0,
            cold_streak: 0,
            last_eval_date: None,
            created_at,
        })
    }

    /// Delete a subscription, returning whether a row was removed.
    pub async fn remove(&self, threema_id: ThreemaId, sensor_id: SensorId) -> Result<bool> {
        let affected =
            sqlx::query("DELETE FROM subscriptions WHERE threema_id = ? AND sensor_id = ?")
                .bind(threema_id.as_str())
                .bind(sensor_id)
                .execute(&self.pool)
                .await
                .context("deleting subscription")?
                .rows_affected();
        Ok(affected > 0)
    }

    /// Fetch a single subscription by user and sensor.
    pub async fn get(
        &self,
        threema_id: ThreemaId,
        sensor_id: SensorId,
    ) -> Result<Option<Subscription>> {
        let sql = format!(
            "SELECT {SUBSCRIPTION_COLUMNS} FROM subscriptions WHERE threema_id = ? AND sensor_id = ?"
        );
        sqlx::query_as::<_, Subscription>(&sql)
            .bind(threema_id.as_str())
            .bind(sensor_id)
            .fetch_optional(&self.pool)
            .await
            .context("fetching subscription")
    }

    /// List all subscriptions belonging to one user, ordered by sensor.
    pub async fn list_for_user(&self, threema_id: ThreemaId) -> Result<Vec<Subscription>> {
        let sql = format!(
            "SELECT {SUBSCRIPTION_COLUMNS} FROM subscriptions WHERE threema_id = ? ORDER BY sensor_id"
        );
        sqlx::query_as::<_, Subscription>(&sql)
            .bind(threema_id.as_str())
            .fetch_all(&self.pool)
            .await
            .context("listing user subscriptions")
    }

    /// List every subscription, ordered by `uid` — used by the daily evaluation sweep.
    pub async fn list_active(&self) -> Result<Vec<Subscription>> {
        let sql = format!("SELECT {SUBSCRIPTION_COLUMNS} FROM subscriptions ORDER BY uid");
        sqlx::query_as::<_, Subscription>(&sql)
            .fetch_all(&self.pool)
            .await
            .context("listing all subscriptions")
    }

    /// Total number of subscriptions.
    pub async fn count(&self) -> Result<u32> {
        let row: (u32,) = sqlx::query_as("SELECT COUNT(*) FROM subscriptions")
            .fetch_one(&self.pool)
            .await
            .context("counting subscriptions")?;
        Ok(row.0)
    }

    /// Persist the alert state machine's outcome for one subscription after a daily evaluation.
    pub async fn update_state(
        &self,
        uid: i64,
        status: SubscriptionStatus,
        warm_streak: u32,
        cold_streak: u32,
        last_eval_date: NaiveDate,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE subscriptions \
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
        .context("updating subscription state")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory store, backed by an in-memory [`Database`] whose schema persists for the test.
    async fn memory_store() -> SubscriptionStore {
        SubscriptionStore::new(&Database::connect_in_memory().await)
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
            assert_eq!(created.status, SubscriptionStatus::Watching);
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
            assert_eq!(fetched.status, SubscriptionStatus::Watching);

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
    }

    mod update_state {
        use super::*;

        #[tokio::test]
        async fn persists_status_streaks_and_date() {
            let store = memory_store().await;
            let sub = store.add(tid("ABCD1234"), SensorId(7), 23.0).await.unwrap();
            let date = NaiveDate::from_ymd_opt(2026, 6, 13).unwrap();
            store
                .update_state(sub.uid, SubscriptionStatus::Notified, 0, 2, date)
                .await
                .unwrap();

            let active = store.list_active().await.unwrap();
            assert_eq!(active.len(), 1);
            let updated = &active[0];
            assert_eq!(updated.status, SubscriptionStatus::Notified);
            assert_eq!(updated.warm_streak, 0);
            assert_eq!(updated.cold_streak, 2);
            assert_eq!(updated.last_eval_date, Some(date));
        }
    }
}
