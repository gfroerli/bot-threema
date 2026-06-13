//! SQLite database handling: Connection pooling and migrations.
//!
//! [`Database`] owns the connection pool and applies the embedded migrations in `migrations/`.
//! Domain-specific query layers (e.g. [`SubscriptionStore`](crate::store::SubscriptionStore))
//! borrow its pool rather than each managing their own connection, so connection and schema
//! handling stays in one place as more tables are added.

use anyhow::{Context, Result};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

/// A handle to the SQLite database.
///
/// Cheaply cloneable - the inner [`SqlitePool`] is reference-counted - so it can be shared across
/// the message handler and the background scheduler.
#[derive(Debug, Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Open (creating if necessary) the SQLite database at `path` and run all migrations.
    pub async fn connect(path: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        Self::from_options(options, 5).await
    }

    /// Build a database from explicit connection options, capping the pool size.
    ///
    /// Shared by [`connect`](Self::connect) and the in-memory test constructor so both run the
    /// identical migration path.
    async fn from_options(options: SqliteConnectOptions, max_connections: u32) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await
            .context("connecting to database")?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("running database migrations")?;
        Ok(Self { pool })
    }

    /// The underlying connection pool, for domain-specific query layers.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// An in-memory database with a single connection, so the schema persists for the life of the
    /// pool. Shared by the database and store tests.
    #[cfg(test)]
    pub(crate) async fn connect_in_memory() -> Self {
        use std::str::FromStr;

        let options = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        Self::from_options(options, 1).await.unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod connect {
        use super::*;

        #[tokio::test]
        async fn applies_migrations_to_empty_database() {
            let db = Database::connect_in_memory().await;
            // The `subscriptions` table from the migrations must exist and be empty.
            let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM subscriptions")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(count.0, 0);
        }
    }
}
