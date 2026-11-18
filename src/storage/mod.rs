mod collections;
mod quizzes;
mod users;
mod words;

use std::{path::Path, time::Duration};

pub use collections::{
    CancelledQuiz, Collection, CollectionError, CollectionName, CollectionNameError,
    CollectionSummary,
};
pub use quizzes::{AnswerOutcome, Candidate, CollectionStats, HardWord, OpenQuiz};
use sqlx::{
    Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
pub use users::User;
pub use words::{ImportSummary, WordView};

use crate::config::LimitsConfig;

/// Caps on user data, checked inside the transactions that write it.
#[derive(Clone, Copy, Debug)]
pub struct StorageLimits {
    pub max_words_per_collection: usize,
    pub max_translations_per_word: usize,
    pub max_collections_per_user: usize,
}

impl From<&LimitsConfig> for StorageLimits {
    fn from(limits: &LimitsConfig) -> StorageLimits {
        StorageLimits {
            max_words_per_collection: limits.max_words_per_collection,
            max_translations_per_word: limits.max_translations_per_word,
            max_collections_per_user: limits.max_collections_per_user,
        }
    }
}

#[derive(Clone)]
pub struct Storage {
    pool: SqlitePool,
    limits: StorageLimits,
}

impl Storage {
    pub async fn connect(path: &Path, limits: StorageLimits) -> Result<Storage, sqlx::Error> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new().connect_with(options).await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Storage { pool, limits })
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Takes the write lock up front: a deferred transaction that reads and
    /// then writes fails with SQLITE_BUSY_SNAPSHOT, which `busy_timeout` does
    /// not retry.
    async fn begin_write(&self) -> Result<Transaction<'static, Sqlite>, sqlx::Error> {
        self.pool.begin_with("BEGIN IMMEDIATE").await
    }

    /// One connection that never expires: every new in-memory connection
    /// would otherwise get its own empty database.
    #[cfg(test)]
    pub async fn in_memory() -> Storage {
        Storage::in_memory_with_limits(StorageLimits::from(&LimitsConfig::default())).await
    }

    #[cfg(test)]
    pub async fn in_memory_with_limits(limits: StorageLimits) -> Storage {
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
        Storage { pool, limits }
    }
}
