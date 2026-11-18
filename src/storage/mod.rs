mod collections;
mod quizzes;
mod users;
mod words;

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

pub use collections::{CancelledQuiz, Collection, CollectionSummary};
pub use users::User;
pub use words::{ImportSummary, WordView};

#[derive(Clone)]
pub struct Storage {
    pool: SqlitePool,
}

impl Storage {
    pub async fn connect(path: &Path) -> Result<Storage, sqlx::Error> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new().connect_with(options).await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Storage { pool })
    }

    /// Takes the write lock up front: a deferred transaction that reads and then
    /// writes fails with SQLITE_BUSY_SNAPSHOT, which `busy_timeout` does not retry.
    async fn begin_write(&self) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, sqlx::Error> {
        self.pool.begin_with("BEGIN IMMEDIATE").await
    }

    /// One connection that never expires: every new in-memory connection
    /// would otherwise get its own empty database.
    #[cfg(test)]
    pub async fn in_memory() -> Storage {
        let options = "sqlite::memory:"
            .parse::<SqliteConnectOptions>()
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        Storage { pool }
    }
}

/// Reports a stored value that violates a domain invariant as a decode error.
fn decode_err<E: std::error::Error + Send + Sync + 'static>(e: E) -> sqlx::Error {
    sqlx::Error::Decode(Box::new(e))
}
