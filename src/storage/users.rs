use sqlx::SqliteConnection;

use super::Storage;
use crate::{
    ids::{CollectionId, UserId},
    period::Period,
    time::Timestamp,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct User {
    pub id: UserId,
    #[sqlx(rename = "period_secs")]
    pub period: Period,
    pub paused: bool,
    pub next_quiz_at: Timestamp,
    pub active_collection_id: Option<CollectionId>,
}

impl User {
    /// Whether the scheduler may send a quiz: not paused and the next quiz is
    /// scheduled at or before `now`. An open quiz is checked separately.
    pub fn is_due(self, now: Timestamp) -> bool {
        !self.paused && self.next_quiz_at <= now
    }
}

impl Storage {
    /// Creates the user as due immediately; an existing user is left untouched.
    pub async fn ensure_user(
        &self,
        id: UserId,
        default_period: Period,
        now: Timestamp,
    ) -> Result<User, sqlx::Error> {
        sqlx::query(
            "INSERT INTO users (id, period_secs, paused, next_quiz_at) VALUES (?, ?, 0, ?) \
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(id)
        .bind(default_period.as_secs())
        .bind(now)
        .execute(&self.pool)
        .await?;
        sqlx::query_as(USER_SELECT_BY_ID)
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn get_user(&self, id: UserId) -> Result<Option<User>, sqlx::Error> {
        sqlx::query_as(USER_SELECT_BY_ID)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// Returns whether a quiz is open; the next quiz is rescheduled only if
    /// none is.
    pub async fn set_period(
        &self,
        id: UserId,
        period: Period,
        now: Timestamp,
    ) -> Result<bool, sqlx::Error> {
        let mut transaction = self.begin_write().await?;
        let open = Self::has_open_quiz(&mut transaction, id).await?;
        sqlx::query(
            "UPDATE users SET period_secs = ?1, \
             next_quiz_at = CASE WHEN ?2 THEN next_quiz_at ELSE ?3 END WHERE id = ?4",
        )
        .bind(period.as_secs())
        .bind(open)
        .bind(now + period)
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(open)
    }

    /// Returns whether a quiz is open; resuming reschedules only if none is.
    pub async fn set_paused(
        &self,
        id: UserId,
        paused: bool,
        now: Timestamp,
    ) -> Result<bool, sqlx::Error> {
        let mut transaction = self.begin_write().await?;
        let open = Self::has_open_quiz(&mut transaction, id).await?;
        let updated = sqlx::query("UPDATE users SET paused = ? WHERE id = ?")
            .bind(paused)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        if updated.rows_affected() == 0 {
            return Ok(false);
        }
        if !paused && !open {
            Self::reschedule(&mut transaction, id, now).await?;
        }
        transaction.commit().await?;
        Ok(open)
    }

    /// Moves the user's next quiz one period after `now`.
    pub async fn postpone_user(&self, id: UserId, now: Timestamp) -> Result<(), sqlx::Error> {
        let mut transaction = self.begin_write().await?;
        Self::reschedule(&mut transaction, id, now).await?;
        transaction.commit().await
    }

    /// Schedules the user's next quiz one period after `now`. Takes a
    /// transaction so the period read and the update cannot interleave with
    /// another writer.
    pub(super) async fn reschedule(
        transaction: &mut SqliteConnection,
        user_id: UserId,
        now: Timestamp,
    ) -> Result<(), sqlx::Error> {
        let period: Period = sqlx::query_scalar("SELECT period_secs FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(&mut *transaction)
            .await?;
        sqlx::query("UPDATE users SET next_quiz_at = ? WHERE id = ?")
            .bind(now + period)
            .bind(user_id)
            .execute(&mut *transaction)
            .await?;
        Ok(())
    }

    pub async fn due_users(&self, now: Timestamp) -> Result<Vec<UserId>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT id FROM users WHERE paused = 0 AND next_quiz_at <= ? \
             AND NOT EXISTS (SELECT 1 FROM quizzes q WHERE q.user_id = users.id AND q.answered_at IS NULL) \
             ORDER BY next_quiz_at, id",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await
    }

    async fn has_open_quiz(
        transaction: &mut SqliteConnection,
        user_id: UserId,
    ) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM quizzes WHERE user_id = ? AND answered_at IS NULL)",
        )
        .bind(user_id)
        .fetch_one(transaction)
        .await
    }
}

const USER_SELECT_BY_ID: &str = "SELECT id, period_secs, paused, next_quiz_at, active_collection_id \
                                 FROM users WHERE id = ?";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::testing::*;

    fn period(secs: u64) -> Period {
        Period::from_secs(secs).unwrap()
    }

    #[tokio::test]
    async fn new_user_is_due_immediately() {
        let storage = Storage::in_memory().await;
        let user = storage
            .ensure_user(uid(1), period(86400), ts(100))
            .await
            .unwrap();
        assert_eq!(user.next_quiz_at, ts(100));
        assert_eq!(storage.due_users(ts(100)).await.unwrap(), [uid(1)]);
        assert!(storage.due_users(ts(99)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ensure_user_keeps_existing_settings() {
        let storage = Storage::in_memory().await;
        storage
            .ensure_user(uid(1), period(86400), ts(100))
            .await
            .unwrap();
        storage
            .set_period(uid(1), period(60), ts(200))
            .await
            .unwrap();
        assert_eq!(
            storage
                .ensure_user(uid(1), period(86400), ts(300))
                .await
                .unwrap()
                .period,
            period(60)
        );
    }

    #[tokio::test]
    async fn period_and_resume_reschedule() {
        let storage = Storage::in_memory().await;
        storage
            .ensure_user(uid(1), period(86400), ts(0))
            .await
            .unwrap();
        assert!(
            !storage
                .set_period(uid(1), period(60), ts(100))
                .await
                .unwrap()
        );
        assert_eq!(
            storage
                .get_user(uid(1))
                .await
                .unwrap()
                .unwrap()
                .next_quiz_at,
            ts(160)
        );
        storage.set_paused(uid(1), true, ts(110)).await.unwrap();
        assert!(storage.due_users(ts(1000)).await.unwrap().is_empty());
        storage.set_paused(uid(1), false, ts(200)).await.unwrap();
        assert_eq!(
            storage
                .get_user(uid(1))
                .await
                .unwrap()
                .unwrap()
                .next_quiz_at,
            ts(260)
        );
    }

    #[tokio::test]
    async fn resuming_unknown_user_is_noop() {
        let storage = Storage::in_memory().await;
        assert!(!storage.set_paused(uid(9), false, ts(0)).await.unwrap());
        assert!(storage.get_user(uid(9)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn open_quiz_blocks_reschedule_and_due() {
        let storage = Storage::in_memory().await;
        storage
            .ensure_user(uid(1), period(60), ts(0))
            .await
            .unwrap();
        sqlx::query("INSERT INTO collections (id, code, name, created_at) VALUES (1, 'C', 'C', 0)")
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO words (id, collection_id, term, term_norm) VALUES (1, 1, 'a', 'a')",
        )
        .execute(&storage.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, sent_at) \
             VALUES (1, 1, '[]', 0, 0)",
        )
        .execute(&storage.pool)
        .await
        .unwrap();
        assert!(storage.due_users(ts(1000)).await.unwrap().is_empty());
        assert!(
            storage
                .set_period(uid(1), period(30), ts(500))
                .await
                .unwrap()
        );
        assert_eq!(
            storage
                .get_user(uid(1))
                .await
                .unwrap()
                .unwrap()
                .next_quiz_at,
            ts(0)
        );
    }
}
