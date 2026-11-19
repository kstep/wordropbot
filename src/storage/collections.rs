use sqlx::{SqliteConnection, SqliteExecutor};

use super::{Storage, quizzes::QuizScope};
use crate::{
    code::Code,
    ids::{CollectionId, MessageId, UserId},
    import::Normalized,
    time::Timestamp,
};

/// A collection name that is not empty and fits the character limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionName(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CollectionNameError {
    #[error("collection name is empty")]
    Empty,
    #[error("collection name is longer than {max} characters")]
    TooLong { max: usize },
}

impl CollectionName {
    pub fn new(name: &str, max_chars: usize) -> Result<CollectionName, CollectionNameError> {
        if Normalized::new(name).as_str().is_empty() {
            return Err(CollectionNameError::Empty);
        }
        if name.chars().count() > max_chars {
            return Err(CollectionNameError::TooLong { max: max_chars });
        }
        Ok(CollectionName(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    #[error("storage: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("user already has the maximum of {max} collections")]
    LimitReached { max: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Collection {
    pub id: CollectionId,
    pub code: Code,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CollectionSummary {
    pub id: CollectionId,
    pub code: Code,
    pub name: String,
    pub word_count: i64,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
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
    ) -> Result<Option<Collection>, CollectionError> {
        let mut transaction = self.begin_write().await?;
        self.check_collection_limit(&mut transaction, user_id)
            .await?;
        let collection: Option<Collection> = sqlx::query_as(
            "INSERT INTO collections (code, name, created_at) VALUES (?, ?, ?) \
             ON CONFLICT(code) DO NOTHING RETURNING id, code, name",
        )
        .bind(code.as_str())
        .bind(name)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(collection) = collection else {
            return Ok(None);
        };
        Self::add_membership_and_activate(&mut transaction, user_id, collection.id, now).await?;
        transaction.commit().await?;
        Ok(Some(collection))
    }

    /// Joins (or re-activates an existing membership of) the collection with
    /// this code. Only a new membership counts against the limit.
    pub async fn join_collection(
        &self,
        user_id: UserId,
        code: &Code,
        now: Timestamp,
    ) -> Result<Option<Collection>, CollectionError> {
        let mut transaction = self.begin_write().await?;
        let collection: Option<Collection> =
            sqlx::query_as("SELECT id, code, name FROM collections WHERE code = ?")
                .bind(code.as_str())
                .fetch_optional(&mut *transaction)
                .await?;
        let Some(collection) = collection else {
            return Ok(None);
        };
        if !Self::is_member(&mut *transaction, user_id, collection.id).await? {
            self.check_collection_limit(&mut transaction, user_id)
                .await?;
        }
        Self::add_membership_and_activate(&mut transaction, user_id, collection.id, now).await?;
        transaction.commit().await?;
        Ok(Some(collection))
    }

    async fn check_collection_limit(
        &self,
        transaction: &mut SqliteConnection,
        user_id: UserId,
    ) -> Result<(), CollectionError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memberships WHERE user_id = ?")
            .bind(user_id)
            .fetch_one(transaction)
            .await?;
        let max = self.limits.max_collections_per_user;
        if count >= max as i64 {
            return Err(CollectionError::LimitReached { max });
        }
        Ok(())
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

    pub async fn has_collections(&self, user_id: UserId) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM memberships WHERE user_id = ?)")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
    }

    pub(super) async fn is_member<'e>(
        executor: impl SqliteExecutor<'e>,
        user_id: UserId,
        collection_id: CollectionId,
    ) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM memberships WHERE user_id = ? AND collection_id = ?)",
        )
        .bind(user_id)
        .bind(collection_id)
        .fetch_one(executor)
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

    /// Returns `None` if the user is not a member. Answered quizzes are kept so
    /// the history returns on rejoin; the collection is deleted with its last
    /// member.
    pub async fn leave_collection(
        &self,
        user_id: UserId,
        collection_id: CollectionId,
        now: Timestamp,
    ) -> Result<Option<Vec<CancelledQuiz>>, sqlx::Error> {
        let mut transaction = self.begin_write().await?;
        if !Self::is_member(&mut *transaction, user_id, collection_id).await? {
            return Ok(None);
        }
        let cancelled = Self::cancel_open_quizzes(
            &mut transaction,
            QuizScope::UserInCollection {
                user_id,
                collection_id,
            },
            now,
        )
        .await?;
        sqlx::query(
            "UPDATE users SET active_collection_id = NULL WHERE id = ? AND active_collection_id = ?",
        )
        .bind(user_id)
        .bind(collection_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM memberships WHERE user_id = ? AND collection_id = ?")
            .bind(user_id)
            .bind(collection_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "DELETE FROM collections WHERE id = ?1 \
             AND NOT EXISTS (SELECT 1 FROM memberships WHERE collection_id = ?1)",
        )
        .bind(collection_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some(cancelled))
    }

    async fn add_membership_and_activate(
        transaction: &mut SqliteConnection,
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
        .execute(&mut *transaction)
        .await?;
        sqlx::query("UPDATE users SET active_collection_id = ? WHERE id = ?")
            .bind(collection_id)
            .bind(user_id)
            .execute(&mut *transaction)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{storage::StorageLimits, testing::*};

    fn code(text: &str) -> Code {
        text.parse().unwrap()
    }

    #[test]
    fn name_must_be_non_empty_and_within_limit() {
        assert_eq!(
            CollectionName::new("Греческий", 9).unwrap().as_str(),
            "Греческий"
        );
        assert_eq!(
            CollectionName::new("Греческий", 8),
            Err(CollectionNameError::TooLong { max: 8 })
        );
        assert_eq!(CollectionName::new("", 8), Err(CollectionNameError::Empty));
        for blank in ["  ", "\u{200b}", " \u{ad} "] {
            assert_eq!(
                CollectionName::new(blank, 8),
                Err(CollectionNameError::Empty),
                "{blank:?}"
            );
        }
    }

    #[tokio::test]
    async fn collection_limit_applies_to_new_memberships_only() {
        let storage = Storage::in_memory_with_limits(StorageLimits {
            max_words_per_collection: 5000,
            max_translations_per_word: 20,
            max_collections_per_user: 1,
        })
        .await;
        for user in [user_id(1), user_id(2)] {
            add_user(&storage, user).await;
        }
        let first = storage
            .create_collection(user_id(1), "A", &code("K7QM-3XPA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        storage
            .create_collection(user_id(2), "B", &code("AAAA-AAAA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            storage
                .create_collection(user_id(1), "C", &code("BBBB-BBBB"), timestamp(0))
                .await,
            Err(CollectionError::LimitReached { max: 1 })
        ));
        assert!(matches!(
            storage
                .join_collection(user_id(1), &code("AAAA-AAAA"), timestamp(0))
                .await,
            Err(CollectionError::LimitReached { max: 1 })
        ));
        assert_eq!(storage.list_collections(user_id(1)).await.unwrap().len(), 1);
        // Joining a collection the user is already in adds nothing.
        assert!(
            storage
                .join_collection(user_id(1), &code("K7QM-3XPA"), timestamp(0))
                .await
                .unwrap()
                .is_some()
        );
        storage
            .leave_collection(user_id(1), first.id, timestamp(0))
            .await
            .unwrap()
            .unwrap();
        assert!(
            storage
                .join_collection(user_id(1), &code("AAAA-AAAA"), timestamp(0))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn join_makes_collection_active() {
        let storage = Storage::in_memory().await;
        add_user(&storage, user_id(1)).await;
        add_user(&storage, user_id(2)).await;
        let collection = storage
            .create_collection(user_id(1), "Греческий", &code("K7QM-3XPA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            storage
                .join_collection(user_id(2), &code("K7QM-3XPA"), timestamp(0))
                .await
                .unwrap()
                .unwrap()
                .id,
            collection.id
        );
        assert_eq!(
            storage
                .get_user(user_id(2))
                .await
                .unwrap()
                .unwrap()
                .active_collection_id,
            Some(collection.id)
        );
        assert!(
            storage
                .join_collection(user_id(2), &code("AAAA-AAAA"), timestamp(0))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn duplicate_code_returns_none() {
        let storage = Storage::in_memory().await;
        add_user(&storage, user_id(1)).await;
        storage
            .create_collection(user_id(1), "A", &code("K7QM-3XPA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        assert!(
            storage
                .create_collection(user_id(1), "B", &code("K7QM-3XPA"), timestamp(0))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn last_member_leaving_deletes_collection() {
        let storage = Storage::in_memory().await;
        add_user(&storage, user_id(1)).await;
        add_user(&storage, user_id(2)).await;
        let collection = storage
            .create_collection(user_id(1), "A", &code("K7QM-3XPA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        storage
            .join_collection(user_id(2), &code("K7QM-3XPA"), timestamp(0))
            .await
            .unwrap();
        storage
            .leave_collection(user_id(1), collection.id, timestamp(0))
            .await
            .unwrap()
            .unwrap();
        assert!(
            storage
                .join_collection(user_id(1), &code("K7QM-3XPA"), timestamp(0))
                .await
                .unwrap()
                .is_some()
        );
        storage
            .leave_collection(user_id(1), collection.id, timestamp(0))
            .await
            .unwrap()
            .unwrap();
        storage
            .leave_collection(user_id(2), collection.id, timestamp(0))
            .await
            .unwrap()
            .unwrap();
        assert!(
            storage
                .join_collection(user_id(1), &code("K7QM-3XPA"), timestamp(0))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn non_member_cannot_touch_collection() {
        let storage = Storage::in_memory().await;
        add_user(&storage, user_id(1)).await;
        add_user(&storage, user_id(2)).await;
        let collection = storage
            .create_collection(user_id(1), "A", &code("K7QM-3XPA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        assert!(
            !storage
                .set_active_collection(user_id(2), collection.id)
                .await
                .unwrap()
        );
        assert!(
            storage
                .leave_collection(user_id(2), collection.id, timestamp(0))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn rejoin_keeps_membership_and_reactivates() {
        let storage = Storage::in_memory().await;
        add_user(&storage, user_id(1)).await;
        let first_collection = storage
            .create_collection(user_id(1), "A", &code("K7QM-3XPA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        storage
            .create_collection(user_id(1), "B", &code("AAAA-AAAA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        storage
            .join_collection(user_id(1), &code("K7QM-3XPA"), timestamp(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            storage
                .get_user(user_id(1))
                .await
                .unwrap()
                .unwrap()
                .active_collection_id,
            Some(first_collection.id)
        );
        let list = storage.list_collections(user_id(1)).await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list.iter()
                .any(|collection| collection.id == first_collection.id
                    && collection.is_active
                    && collection.word_count == 0)
        );
    }

    #[tokio::test]
    async fn leaving_cancels_open_quiz_of_this_collection_only() {
        let storage = Storage::in_memory().await;
        add_user(&storage, user_id(1)).await;
        let first_collection = storage
            .create_collection(user_id(1), "A", &code("K7QM-3XPA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        let second_collection = storage
            .create_collection(user_id(1), "B", &code("AAAA-AAAA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        storage
            .import_pairs(first_collection.id, &[pair("a", "б")])
            .await
            .unwrap();
        let word = storage
            .find_word(first_collection.id, "a")
            .await
            .unwrap()
            .unwrap();
        let quiz = storage
            .create_quiz(user_id(1), word.id, &["б".into()], 0, timestamp(0))
            .await
            .unwrap()
            .unwrap();
        storage
            .set_quiz_message(quiz, message_id(42))
            .await
            .unwrap();
        // Leaving B (the active one) must not touch the quiz from A.
        let cancelled = storage
            .leave_collection(user_id(1), second_collection.id, timestamp(100))
            .await
            .unwrap()
            .unwrap();
        assert!(cancelled.is_empty());
        assert_eq!(
            storage
                .get_user(user_id(1))
                .await
                .unwrap()
                .unwrap()
                .active_collection_id,
            None
        );
        let cancelled = storage
            .leave_collection(user_id(1), first_collection.id, timestamp(100))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            cancelled,
            [CancelledQuiz {
                user_id: user_id(1),
                message_id: Some(message_id(42))
            }]
        );
        assert_eq!(next_quiz_at(&storage, user_id(1)).await, timestamp(160));
    }

    #[tokio::test]
    async fn list_works_without_active_collection() {
        let storage = Storage::in_memory().await;
        add_user(&storage, user_id(1)).await;
        let first_collection = storage
            .create_collection(user_id(1), "A", &code("K7QM-3XPA"), timestamp(0))
            .await
            .unwrap()
            .unwrap();
        let second_collection = storage
            .create_collection(user_id(1), "B", &code("AAAA-AAAA"), timestamp(1))
            .await
            .unwrap()
            .unwrap();
        storage
            .leave_collection(user_id(1), second_collection.id, timestamp(2))
            .await
            .unwrap()
            .unwrap();
        let list = storage.list_collections(user_id(1)).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, first_collection.id);
        assert!(!list[0].is_active);
    }
}
