mod collections;
mod quizzes;
mod users;
mod words;

use std::error::Error;
use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

pub use collections::{CancelledQuiz, Collection, CollectionSummary};
pub use quizzes::{AnswerOutcome, Candidate, CollectionStats, HardWord, OpenQuiz};
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

    pub async fn close(&self) {
        self.pool.close().await;
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
        let options: SqliteConnectOptions = "sqlite::memory:".parse().unwrap();
        let options = options.foreign_keys(true);
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

/// A stored value that violates a domain invariant, reported as a decode error.
#[derive(Debug)]
struct StoredValueError(Box<dyn Error + Send + Sync>);

impl StoredValueError {
    fn new<E: Error + Send + Sync + 'static>(source: E) -> StoredValueError {
        StoredValueError(Box::new(source))
    }
}

impl From<StoredValueError> for sqlx::Error {
    fn from(error: StoredValueError) -> sqlx::Error {
        Self::Decode(error.0)
    }
}

#[cfg(test)]
mod testing {
    use crate::ids::{MessageId, UserId};
    use crate::time::Timestamp;

    pub fn ts(secs: i64) -> Timestamp {
        Timestamp::from_secs(secs)
    }
    pub fn uid(id: i64) -> UserId {
        UserId(id)
    }
    pub fn mid(id: i32) -> MessageId {
        MessageId(id)
    }
}
