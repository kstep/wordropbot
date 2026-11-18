use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::Storage;
use crate::code::Code;
use crate::ids::{CollectionId, MessageId, UserId};
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: CollectionId,
    pub code: Code,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionSummary {
    pub id: CollectionId,
    pub code: Code,
    pub name: String,
    pub word_count: i64,
    pub is_active: bool,
}

impl<'r> sqlx::FromRow<'r, SqliteRow> for Collection {
    fn from_row(row: &'r SqliteRow) -> Result<Collection, sqlx::Error> {
        Ok(Collection {
            id: row.try_get("id")?,
            code: row.try_get("code")?,
            name: row.try_get("name")?,
        })
    }
}

impl<'r> sqlx::FromRow<'r, SqliteRow> for CollectionSummary {
    fn from_row(row: &'r SqliteRow) -> Result<CollectionSummary, sqlx::Error> {
        Ok(CollectionSummary {
            id: row.try_get("id")?,
            code: row.try_get("code")?,
            name: row.try_get("name")?,
            word_count: row.try_get("word_count")?,
            is_active: row.try_get("is_active")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledQuiz {
    pub user_id: UserId,
    pub message_id: Option<MessageId>,
}

impl Storage {
    /// Creates the collection, the creator's membership and makes it active.
    /// Returns `None` if the code is already taken.
    pub async fn create_collection(
        &self,
        user_id: UserId,
        name: &str,
        code: &Code,
        now: Timestamp,
    ) -> Result<Option<Collection>, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let collection: Option<Collection> = sqlx::query_as(
            "INSERT INTO collections (code, name, created_at) VALUES (?, ?, ?) \
             ON CONFLICT(code) DO NOTHING RETURNING id, code, name",
        )
        .bind(code.as_str())
        .bind(name)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(collection) = collection else {
            return Ok(None);
        };
        Self::add_membership_and_activate(&mut tx, user_id, collection.id, now).await?;
        tx.commit().await?;
        Ok(Some(collection))
    }

    /// Joins (or re-activates an existing membership of) the collection with this code.
    pub async fn join_collection(
        &self,
        user_id: UserId,
        code: &Code,
        now: Timestamp,
    ) -> Result<Option<Collection>, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let collection: Option<Collection> =
            sqlx::query_as("SELECT id, code, name FROM collections WHERE code = ?")
                .bind(code.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        let Some(collection) = collection else {
            return Ok(None);
        };
        Self::add_membership_and_activate(&mut tx, user_id, collection.id, now).await?;
        tx.commit().await?;
        Ok(Some(collection))
    }

    pub async fn list_collections(
        &self,
        user_id: UserId,
    ) -> Result<Vec<CollectionSummary>, sqlx::Error> {
        sqlx::query_as(
            "SELECT c.id, c.code, c.name, \
                    (SELECT COUNT(*) FROM words w WHERE w.collection_id = c.id) AS word_count, \
                    COALESCE(u.active_collection_id = c.id, 0) AS is_active \
             FROM memberships m \
             JOIN collections c ON c.id = m.collection_id \
             JOIN users u ON u.id = m.user_id \
             WHERE m.user_id = ? ORDER BY m.joined_at, c.id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn is_member(
        &self,
        user_id: UserId,
        collection_id: CollectionId,
    ) -> Result<bool, sqlx::Error> {
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
        user_id: UserId,
        collection_id: CollectionId,
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
        user_id: UserId,
        collection_id: CollectionId,
        now: Timestamp,
    ) -> Result<Option<Vec<CancelledQuiz>>, sqlx::Error> {
        let mut tx = self.begin_write().await?;
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

        let cancelled: Vec<(UserId, Option<MessageId>)> = sqlx::query_as(
            "DELETE FROM quizzes WHERE user_id = ?1 AND answered_at IS NULL \
             AND word_id IN (SELECT id FROM words WHERE collection_id = ?2) \
             RETURNING user_id, message_id",
        )
        .bind(user_id)
        .bind(collection_id)
        .fetch_all(&mut *tx)
        .await?;
        if !cancelled.is_empty() {
            Storage::reschedule(&mut tx, user_id, now).await?;
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

    async fn add_membership_and_activate(
        tx: &mut sqlx::SqliteConnection,
        user_id: UserId,
        collection_id: CollectionId,
        now: Timestamp,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::period::Period;
    use crate::storage::testing::*;

    fn code(s: &str) -> Code {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn join_makes_collection_active() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        s.ensure_user(uid(2), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        let c = s
            .create_collection(uid(1), "Греческий", &code("K7QM-3XPA"), ts(0))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            s.join_collection(uid(2), &code("K7QM-3XPA"), ts(0))
                .await
                .unwrap()
                .unwrap()
                .id,
            c.id
        );
        assert_eq!(
            s.get_user(uid(2))
                .await
                .unwrap()
                .unwrap()
                .active_collection_id,
            Some(c.id)
        );
        assert!(
            s.join_collection(uid(2), &code("AAAA-AAAA"), ts(0))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn duplicate_code_returns_none() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        s.create_collection(uid(1), "A", &code("K7QM-3XPA"), ts(0))
            .await
            .unwrap()
            .unwrap();
        assert!(
            s.create_collection(uid(1), "B", &code("K7QM-3XPA"), ts(0))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn last_member_leaving_deletes_collection() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        s.ensure_user(uid(2), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        let c = s
            .create_collection(uid(1), "A", &code("K7QM-3XPA"), ts(0))
            .await
            .unwrap()
            .unwrap();
        s.join_collection(uid(2), &code("K7QM-3XPA"), ts(0))
            .await
            .unwrap();
        s.leave_collection(uid(1), c.id, ts(0))
            .await
            .unwrap()
            .unwrap();
        assert!(
            s.join_collection(uid(1), &code("K7QM-3XPA"), ts(0))
                .await
                .unwrap()
                .is_some()
        );
        s.leave_collection(uid(1), c.id, ts(0))
            .await
            .unwrap()
            .unwrap();
        s.leave_collection(uid(2), c.id, ts(0))
            .await
            .unwrap()
            .unwrap();
        assert!(
            s.join_collection(uid(1), &code("K7QM-3XPA"), ts(0))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn non_member_cannot_touch_collection() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        s.ensure_user(uid(2), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        let c = s
            .create_collection(uid(1), "A", &code("K7QM-3XPA"), ts(0))
            .await
            .unwrap()
            .unwrap();
        assert!(!s.set_active_collection(uid(2), c.id).await.unwrap());
        assert!(
            s.leave_collection(uid(2), c.id, ts(0))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn rejoin_keeps_membership_and_reactivates() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        let first_collection = s
            .create_collection(uid(1), "A", &code("K7QM-3XPA"), ts(0))
            .await
            .unwrap()
            .unwrap();
        s.create_collection(uid(1), "B", &code("AAAA-AAAA"), ts(0))
            .await
            .unwrap()
            .unwrap();
        s.join_collection(uid(1), &code("K7QM-3XPA"), ts(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            s.get_user(uid(1))
                .await
                .unwrap()
                .unwrap()
                .active_collection_id,
            Some(first_collection.id)
        );
        let list = s.list_collections(uid(1)).await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list.iter()
                .any(|c| c.id == first_collection.id && c.is_active && c.word_count == 0)
        );
    }

    #[tokio::test]
    async fn leaving_cancels_open_quiz_of_this_collection_only() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        let first_collection = s
            .create_collection(uid(1), "A", &code("K7QM-3XPA"), ts(0))
            .await
            .unwrap()
            .unwrap();
        let second_collection = s
            .create_collection(uid(1), "B", &code("AAAA-AAAA"), ts(0))
            .await
            .unwrap()
            .unwrap();
        sqlx::query(
            "INSERT INTO words (id, collection_id, term, term_norm) VALUES (1, ?, 'a', 'a')",
        )
        .bind(first_collection.id)
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
        let cancelled = s
            .leave_collection(uid(1), second_collection.id, ts(100))
            .await
            .unwrap()
            .unwrap();
        assert!(cancelled.is_empty());
        assert_eq!(
            s.get_user(uid(1))
                .await
                .unwrap()
                .unwrap()
                .active_collection_id,
            None
        );
        let cancelled = s
            .leave_collection(uid(1), first_collection.id, ts(100))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            cancelled,
            [CancelledQuiz {
                user_id: uid(1),
                message_id: Some(mid(42))
            }]
        );
        assert_eq!(
            s.get_user(uid(1)).await.unwrap().unwrap().next_quiz_at,
            ts(160)
        );
    }

    #[tokio::test]
    async fn list_works_without_active_collection() {
        let s = Storage::in_memory().await;
        s.ensure_user(uid(1), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        let first_collection = s
            .create_collection(uid(1), "A", &code("K7QM-3XPA"), ts(0))
            .await
            .unwrap()
            .unwrap();
        let second_collection = s
            .create_collection(uid(1), "B", &code("AAAA-AAAA"), ts(1))
            .await
            .unwrap()
            .unwrap();
        s.leave_collection(uid(1), second_collection.id, ts(2))
            .await
            .unwrap()
            .unwrap();
        let list = s.list_collections(uid(1)).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, first_collection.id);
        assert!(!list[0].is_active);
    }
}
