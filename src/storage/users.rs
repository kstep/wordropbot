use super::Storage;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub period_secs: i64,
    pub paused: bool,
    pub next_quiz_at: i64,
    pub active_collection_id: Option<i64>,
}

impl Storage {
    /// Creates the user as due immediately; an existing user is left untouched.
    pub async fn ensure_user(
        &self,
        id: i64,
        default_period_secs: i64,
        now: i64,
    ) -> Result<User, sqlx::Error> {
        sqlx::query(
            "INSERT INTO users (id, period_secs, paused, next_quiz_at) VALUES (?, ?, 0, ?) \
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(id)
        .bind(default_period_secs)
        .bind(now)
        .execute(&self.pool)
        .await?;
        sqlx::query_as(USER_SELECT_BY_ID)
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn get_user(&self, id: i64) -> Result<Option<User>, sqlx::Error> {
        sqlx::query_as(USER_SELECT_BY_ID)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// Returns whether a quiz is open; the next quiz is rescheduled only if none is.
    pub async fn set_period(&self, id: i64, secs: i64, now: i64) -> Result<bool, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let open = has_open_quiz(&mut tx, id).await?;
        sqlx::query(
            "UPDATE users SET period_secs = ?1, \
             next_quiz_at = CASE WHEN ?2 THEN next_quiz_at ELSE ?3 + ?1 END WHERE id = ?4",
        )
        .bind(secs)
        .bind(open)
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(open)
    }

    /// Returns whether a quiz is open; resuming reschedules only if none is.
    pub async fn set_paused(&self, id: i64, paused: bool, now: i64) -> Result<bool, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let open = has_open_quiz(&mut tx, id).await?;
        sqlx::query(
            "UPDATE users SET paused = ?1, \
             next_quiz_at = CASE WHEN ?1 OR ?2 THEN next_quiz_at ELSE ?3 + period_secs END \
             WHERE id = ?4",
        )
        .bind(paused)
        .bind(open)
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(open)
    }

    pub async fn due_users(&self, now: i64) -> Result<Vec<i64>, sqlx::Error> {
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

async fn has_open_quiz(tx: &mut sqlx::SqliteConnection, user_id: i64) -> Result<bool, sqlx::Error> {
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

    #[tokio::test]
    async fn new_user_is_due_immediately() {
        let s = Storage::in_memory().await;
        let u = s.ensure_user(1, 86400, 100).await.unwrap();
        assert_eq!(u.next_quiz_at, 100);
        assert_eq!(s.due_users(100).await.unwrap(), [1]);
        assert!(s.due_users(99).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ensure_user_keeps_existing_settings() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 86400, 100).await.unwrap();
        s.set_period(1, 60, 200).await.unwrap();
        assert_eq!(s.ensure_user(1, 86400, 300).await.unwrap().period_secs, 60);
    }

    #[tokio::test]
    async fn period_and_resume_reschedule() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 86400, 0).await.unwrap();
        assert!(!s.set_period(1, 60, 100).await.unwrap());
        assert_eq!(s.get_user(1).await.unwrap().unwrap().next_quiz_at, 160);
        s.set_paused(1, true, 110).await.unwrap();
        assert!(s.due_users(1000).await.unwrap().is_empty());
        s.set_paused(1, false, 200).await.unwrap();
        assert_eq!(s.get_user(1).await.unwrap().unwrap().next_quiz_at, 260);
    }

    #[tokio::test]
    async fn open_quiz_blocks_reschedule_and_due() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
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
        assert!(s.due_users(1000).await.unwrap().is_empty());
        assert!(s.set_period(1, 30, 500).await.unwrap());
        assert_eq!(s.get_user(1).await.unwrap().unwrap().next_quiz_at, 0);
    }
}
