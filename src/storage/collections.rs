use super::Storage;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Collection {
    pub id: i64,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CollectionSummary {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub word_count: i64,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledQuiz {
    pub user_id: i64,
    pub message_id: Option<i32>,
}

impl Storage {
    /// Creates the collection, the creator's membership and makes it active.
    /// Returns `None` if the code is already taken.
    pub async fn create_collection(
        &self,
        user_id: i64,
        name: &str,
        code: &str,
        now: i64,
    ) -> Result<Option<Collection>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let collection: Option<Collection> = sqlx::query_as(
            "INSERT INTO collections (code, name, created_at) VALUES (?, ?, ?) \
             ON CONFLICT(code) DO NOTHING RETURNING id, code, name",
        )
        .bind(code)
        .bind(name)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(collection) = collection else {
            return Ok(None);
        };
        add_membership_and_activate(&mut tx, user_id, collection.id, now).await?;
        tx.commit().await?;
        Ok(Some(collection))
    }

    /// Joins (or re-activates an existing membership of) the collection with this code.
    pub async fn join_collection(
        &self,
        user_id: i64,
        code: &str,
        now: i64,
    ) -> Result<Option<Collection>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let collection: Option<Collection> =
            sqlx::query_as("SELECT id, code, name FROM collections WHERE code = ?")
                .bind(code)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(collection) = collection else {
            return Ok(None);
        };
        add_membership_and_activate(&mut tx, user_id, collection.id, now).await?;
        tx.commit().await?;
        Ok(Some(collection))
    }

    pub async fn list_collections(
        &self,
        user_id: i64,
    ) -> Result<Vec<CollectionSummary>, sqlx::Error> {
        sqlx::query_as(
            "SELECT c.id, c.code, c.name, \
                    (SELECT COUNT(*) FROM words w WHERE w.collection_id = c.id) AS word_count, \
                    (u.active_collection_id = c.id) AS is_active \
             FROM memberships m \
             JOIN collections c ON c.id = m.collection_id \
             JOIN users u ON u.id = m.user_id \
             WHERE m.user_id = ? ORDER BY m.joined_at, c.id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn is_member(&self, user_id: i64, collection_id: i64) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM memberships WHERE user_id = ? AND collection_id = ?)",
        )
        .bind(user_id)
        .bind(collection_id)
        .fetch_one(&self.pool)
        .await
    }

    /// Returns `false` if the user is not a member of the collection.
    pub async fn set_active_collection(
        &self,
        user_id: i64,
        collection_id: i64,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE users SET active_collection_id = ?2 WHERE id = ?1 \
             AND EXISTS (SELECT 1 FROM memberships WHERE user_id = ?1 AND collection_id = ?2)",
        )
        .bind(user_id)
        .bind(collection_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Returns `None` if the user is not a member. Answered quizzes are kept so the
    /// history returns on rejoin; the collection is deleted with its last member.
    pub async fn leave_collection(
        &self,
        user_id: i64,
        collection_id: i64,
        now: i64,
    ) -> Result<Option<Vec<CancelledQuiz>>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let member: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM memberships WHERE user_id = ? AND collection_id = ?)",
        )
        .bind(user_id)
        .bind(collection_id)
        .fetch_one(&mut *tx)
        .await?;
        if !member {
            return Ok(None);
        }

        let cancelled: Vec<(i64, Option<i32>)> = sqlx::query_as(
            "DELETE FROM quizzes WHERE user_id = ?1 AND answered_at IS NULL \
             AND word_id IN (SELECT id FROM words WHERE collection_id = ?2) \
             RETURNING user_id, message_id",
        )
        .bind(user_id)
        .bind(collection_id)
        .fetch_all(&mut *tx)
        .await?;
        if !cancelled.is_empty() {
            sqlx::query("UPDATE users SET next_quiz_at = ? + period_secs WHERE id = ?")
                .bind(now)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "UPDATE users SET active_collection_id = NULL WHERE id = ? AND active_collection_id = ?",
        )
        .bind(user_id)
        .bind(collection_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM memberships WHERE user_id = ? AND collection_id = ?")
            .bind(user_id)
            .bind(collection_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM collections WHERE id = ?1 \
             AND NOT EXISTS (SELECT 1 FROM memberships WHERE collection_id = ?1)",
        )
        .bind(collection_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(
            cancelled
                .into_iter()
                .map(|(user_id, message_id)| CancelledQuiz {
                    user_id,
                    message_id,
                })
                .collect(),
        ))
    }
}

async fn add_membership_and_activate(
    tx: &mut sqlx::SqliteConnection,
    user_id: i64,
    collection_id: i64,
    now: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO memberships (user_id, collection_id, joined_at) VALUES (?, ?, ?) \
         ON CONFLICT(user_id, collection_id) DO NOTHING",
    )
    .bind(user_id)
    .bind(collection_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE users SET active_collection_id = ? WHERE id = ?")
        .bind(collection_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn join_makes_collection_active() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        s.ensure_user(2, 60, 0).await.unwrap();
        let c = s
            .create_collection(1, "Греческий", "K7QM-3XPA", 0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            s.join_collection(2, "K7QM-3XPA", 0)
                .await
                .unwrap()
                .unwrap()
                .id,
            c.id
        );
        assert_eq!(
            s.get_user(2).await.unwrap().unwrap().active_collection_id,
            Some(c.id)
        );
        assert!(
            s.join_collection(2, "AAAA-AAAA", 0)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn duplicate_code_returns_none() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        s.create_collection(1, "A", "K7QM-3XPA", 0)
            .await
            .unwrap()
            .unwrap();
        assert!(
            s.create_collection(1, "B", "K7QM-3XPA", 0)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn last_member_leaving_deletes_collection() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        s.ensure_user(2, 60, 0).await.unwrap();
        let c = s
            .create_collection(1, "A", "K7QM-3XPA", 0)
            .await
            .unwrap()
            .unwrap();
        s.join_collection(2, "K7QM-3XPA", 0).await.unwrap();
        s.leave_collection(1, c.id, 0).await.unwrap().unwrap();
        assert!(
            s.join_collection(1, "K7QM-3XPA", 0)
                .await
                .unwrap()
                .is_some()
        );
        s.leave_collection(1, c.id, 0).await.unwrap().unwrap();
        s.leave_collection(2, c.id, 0).await.unwrap().unwrap();
        assert!(
            s.join_collection(1, "K7QM-3XPA", 0)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn non_member_cannot_touch_collection() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        s.ensure_user(2, 60, 0).await.unwrap();
        let c = s
            .create_collection(1, "A", "K7QM-3XPA", 0)
            .await
            .unwrap()
            .unwrap();
        assert!(!s.set_active_collection(2, c.id).await.unwrap());
        assert!(s.leave_collection(2, c.id, 0).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejoin_keeps_membership_and_reactivates() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        let a = s
            .create_collection(1, "A", "K7QM-3XPA", 0)
            .await
            .unwrap()
            .unwrap();
        s.create_collection(1, "B", "AAAA-AAAA", 0)
            .await
            .unwrap()
            .unwrap();
        s.join_collection(1, "K7QM-3XPA", 5).await.unwrap().unwrap();
        assert_eq!(
            s.get_user(1).await.unwrap().unwrap().active_collection_id,
            Some(a.id)
        );
        let list = s.list_collections(1).await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list.iter()
                .any(|c| c.id == a.id && c.is_active && c.word_count == 0)
        );
    }

    #[tokio::test]
    async fn leaving_cancels_open_quiz_of_this_collection_only() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        let a = s
            .create_collection(1, "A", "K7QM-3XPA", 0)
            .await
            .unwrap()
            .unwrap();
        let b = s
            .create_collection(1, "B", "AAAA-AAAA", 0)
            .await
            .unwrap()
            .unwrap();
        sqlx::query(
            "INSERT INTO words (id, collection_id, term, term_norm) VALUES (1, ?, 'a', 'a')",
        )
        .bind(a.id)
        .execute(&s.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, message_id, sent_at) \
             VALUES (1, 1, '[]', 0, 42, 0)",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        // Leaving B (the active one) must not touch the quiz from A.
        let cancelled = s.leave_collection(1, b.id, 100).await.unwrap().unwrap();
        assert!(cancelled.is_empty());
        assert_eq!(
            s.get_user(1).await.unwrap().unwrap().active_collection_id,
            None
        );
        let cancelled = s.leave_collection(1, a.id, 100).await.unwrap().unwrap();
        assert_eq!(
            cancelled,
            [CancelledQuiz {
                user_id: 1,
                message_id: Some(42)
            }]
        );
        assert_eq!(s.get_user(1).await.unwrap().unwrap().next_quiz_at, 160);
    }
}
