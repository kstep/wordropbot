use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use super::Storage;
use crate::{
    ids::{CollectionId, UserId},
    period::Period,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct User {
    pub id: UserId,
    #[sqlx(rename = "period_secs")]
    pub period: Period,
    pub paused: bool,
    pub next_quiz_at: DateTime<Utc>,
    pub active_collection_id: Option<CollectionId>,
}

impl User {
    /// Whether the scheduler may send a quiz: not paused and the next quiz is
    /// scheduled at or before `now`. An open quiz is checked separately.
    pub fn is_due(self, now: DateTime<Utc>) -> bool {
        !self.paused && self.next_quiz_at <= now
    }
}

impl Storage {
    /// Creates the user as due immediately; an existing user is left untouched.
    pub async fn ensure_user(
        &self,
        id: UserId,
        default_period: Period,
        now: DateTime<Utc>,
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
        now: DateTime<Utc>,
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
        now: DateTime<Utc>,
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
    pub async fn postpone_user(&self, id: UserId, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
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
        now: DateTime<Utc>,
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

    pub async fn due_users(&self, now: DateTime<Utc>) -> Result<Vec<UserId>, sqlx::Error> {
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
    use crate::testing::*;

    fn period(seconds: u64) -> Period {
        Period::from_secs(seconds).unwrap()
    }

    /// Moments are stored as text with as many fraction digits as they need.
    #[tokio::test]
    async fn due_check_orders_fractional_seconds() {
        let storage = Storage::in_memory().await;
        let moment = |nanos| DateTime::from_timestamp(100, nanos).unwrap();
        storage
            .ensure_user(user_id(1), period(60), moment(500_000_000))
            .await
            .unwrap();
        for (now, is_due) in [
            (timestamp(100), false),
            (moment(499_999_999), false),
            (moment(500_000_000), true),
            (moment(500_000_001), true),
            (timestamp(101), true),
        ] {
            let due = storage.due_users(now).await.unwrap();
            assert_eq!(!due.is_empty(), is_due, "{now}");
        }
    }

    #[tokio::test]
    async fn new_user_is_due_immediately() {
        let storage = Storage::in_memory().await;
        let user = storage
            .ensure_user(user_id(1), period(86400), timestamp(100))
            .await
            .unwrap();
        assert_eq!(user.next_quiz_at, timestamp(100));
        assert_eq!(
            storage.due_users(timestamp(100)).await.unwrap(),
            [user_id(1)]
        );
        assert!(storage.due_users(timestamp(99)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ensure_user_keeps_existing_settings() {
        let storage = Storage::in_memory().await;
        storage
            .ensure_user(user_id(1), period(86400), timestamp(100))
            .await
            .unwrap();
        storage
            .set_period(user_id(1), period(60), timestamp(200))
            .await
            .unwrap();
        assert_eq!(
            storage
                .ensure_user(user_id(1), period(86400), timestamp(300))
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
            .ensure_user(user_id(1), period(86400), timestamp(0))
            .await
            .unwrap();
        assert!(
            !storage
                .set_period(user_id(1), period(60), timestamp(100))
                .await
                .unwrap()
        );
        assert_eq!(next_quiz_at(&storage, user_id(1)).await, timestamp(160));
        storage
            .set_paused(user_id(1), true, timestamp(110))
            .await
            .unwrap();
        assert!(storage.due_users(timestamp(1000)).await.unwrap().is_empty());
        storage
            .set_paused(user_id(1), false, timestamp(200))
            .await
            .unwrap();
        assert_eq!(next_quiz_at(&storage, user_id(1)).await, timestamp(260));
    }

    #[tokio::test]
    async fn resuming_unknown_user_is_noop() {
        let storage = Storage::in_memory().await;
        assert!(
            !storage
                .set_paused(user_id(9), false, timestamp(0))
                .await
                .unwrap()
        );
        assert!(storage.get_user(user_id(9)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn open_quiz_blocks_reschedule_and_due() {
        let storage = Storage::in_memory().await;
        let collection = user_with_collection(&storage, user_id(1), "K7QM-3XPA").await;
        storage
            .import_pairs(collection, &[pair("a", "б")])
            .await
            .unwrap();
        let word = storage.find_word(collection, "a").await.unwrap().unwrap();
        storage
            .create_quiz(user_id(1), word.id, &["б".into()], 0, timestamp(0))
            .await
            .unwrap()
            .unwrap();
        assert!(storage.due_users(timestamp(1000)).await.unwrap().is_empty());
        assert!(
            storage
                .set_period(user_id(1), period(30), timestamp(500))
                .await
                .unwrap()
        );
        assert_eq!(next_quiz_at(&storage, user_id(1)).await, timestamp(0));
    }
}
