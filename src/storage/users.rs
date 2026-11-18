use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{Storage, decode_err};
use crate::ids::{CollectionId, UserId};
use crate::period::Period;
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: UserId,
    pub period: Period,
    pub paused: bool,
    pub next_quiz_at: Timestamp,
    pub active_collection_id: Option<CollectionId>,
}

#[derive(Debug, thiserror::Error)]
#[error("stored period {0} is out of range")]
struct BadStoredPeriod(i64);

fn period_from_secs(secs: i64) -> Result<Period, sqlx::Error> {
    u64::try_from(secs)
        .ok()
        .and_then(Period::from_secs)
        .ok_or_else(|| decode_err(BadStoredPeriod(secs)))
}

impl<'r> sqlx::FromRow<'r, SqliteRow> for User {
    fn from_row(row: &'r SqliteRow) -> Result<User, sqlx::Error> {
        Ok(User {
            id: row.try_get("id")?,
            period: period_from_secs(row.try_get("period_secs")?)?,
            paused: row.try_get("paused")?,
            next_quiz_at: row.try_get("next_quiz_at")?,
            active_collection_id: row.try_get("active_collection_id")?,
        })
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

    /// Returns whether a quiz is open; the next quiz is rescheduled only if none is.
    pub async fn set_period(
        &self,
        id: UserId,
        period: Period,
        now: Timestamp,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let open = has_open_quiz(&mut tx, id).await?;
        sqlx::query(
            "UPDATE users SET period_secs = ?1, \
             next_quiz_at = CASE WHEN ?2 THEN next_quiz_at ELSE ?3 END WHERE id = ?4",
        )
        .bind(period.as_secs())
        .bind(open)
        .bind(now + period)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(open)
    }

    /// Returns whether a quiz is open; resuming reschedules only if none is.
    pub async fn set_paused(
        &self,
        id: UserId,
        paused: bool,
        now: Timestamp,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let open = has_open_quiz(&mut tx, id).await?;
        sqlx::query("UPDATE users SET paused = ? WHERE id = ?")
            .bind(paused)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if !paused && !open {
            reschedule(&mut tx, id, now).await?;
        }
        tx.commit().await?;
        Ok(open)
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
}

const USER_SELECT_BY_ID: &str = "SELECT id, period_secs, paused, next_quiz_at, active_collection_id \
                                 FROM users WHERE id = ?";

/// Schedules the user's next quiz one period after `now`. Callers run inside a write transaction.
pub(super) async fn reschedule(
    tx: &mut sqlx::SqliteConnection,
    user_id: UserId,
    now: Timestamp,
) -> Result<(), sqlx::Error> {
    let secs: i64 = sqlx::query_scalar("SELECT period_secs FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("UPDATE users SET next_quiz_at = ? WHERE id = ?")
        .bind(now + period_from_secs(secs)?)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

async fn has_open_quiz(
    tx: &mut sqlx::SqliteConnection,
    user_id: UserId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM quizzes WHERE user_id = ? AND answered_at IS NULL)",
    )
    .bind(user_id)
    .fetch_one(tx)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::testing::*;

    fn period(secs: u64) -> Period {
        Period::from_secs(secs).unwrap()
    }

    #[tokio::test]
    async fn new_user_is_due_immediately() {
        let s = Storage::in_memory().await;
        let u = s.ensure_user(uid(1), period(86400), ts(100)).await.unwrap();
        assert_eq!(u.next_quiz_at, ts(100));
        assert_eq!(s.due_users(ts(100)).await.unwrap(), [uid(1)]);
        assert!(s.due_users(ts(99)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ensure_user_keeps_existing_settings() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), period(86400), ts(100)).await.unwrap();
        s.set_period(uid(1), period(60), ts(200)).await.unwrap();
        assert_eq!(
            s.ensure_user(uid(1), period(86400), ts(300))
                .await
                .unwrap()
                .period,
            period(60)
        );
    }

    #[tokio::test]
    async fn period_and_resume_reschedule() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), period(86400), ts(0)).await.unwrap();
        assert!(!s.set_period(uid(1), period(60), ts(100)).await.unwrap());
        assert_eq!(
            s.get_user(uid(1)).await.unwrap().unwrap().next_quiz_at,
            ts(160)
        );
        s.set_paused(uid(1), true, ts(110)).await.unwrap();
        assert!(s.due_users(ts(1000)).await.unwrap().is_empty());
        s.set_paused(uid(1), false, ts(200)).await.unwrap();
        assert_eq!(
            s.get_user(uid(1)).await.unwrap().unwrap().next_quiz_at,
            ts(260)
        );
    }

    #[tokio::test]
    async fn open_quiz_blocks_reschedule_and_due() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), period(60), ts(0)).await.unwrap();
        sqlx::query("INSERT INTO collections (id, code, name, created_at) VALUES (1, 'C', 'C', 0)")
            .execute(&s.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO words (id, collection_id, term, term_norm) VALUES (1, 1, 'a', 'a')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, sent_at) \
             VALUES (1, 1, '[]', 0, 0)",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        assert!(s.due_users(ts(1000)).await.unwrap().is_empty());
        assert!(s.set_period(uid(1), period(30), ts(500)).await.unwrap());
        assert_eq!(
            s.get_user(uid(1)).await.unwrap().unwrap().next_quiz_at,
            ts(0)
        );
    }
}
