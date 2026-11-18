use std::collections::HashSet;

use sqlx::SqliteExecutor;

use super::{Storage, collections::CancelledQuiz, quizzes::QuizScope};
use crate::{
    ids::{CollectionId, UserId, WordId},
    import::{Normalized, ParsedPair},
    time::Timestamp,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub added: u32,
    pub updated: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordView {
    pub id: WordId,
    pub term: String,
    pub translations: Vec<String>,
}

impl Storage {
    /// Merges the pairs into the collection in one transaction. A word counts
    /// as `updated` only if it existed before this import and gained a
    /// translation.
    pub async fn import_pairs(
        &self,
        collection_id: CollectionId,
        pairs: &[ParsedPair],
    ) -> Result<ImportSummary, sqlx::Error> {
        if pairs.is_empty() {
            return Ok(ImportSummary::default());
        }
        let mut transaction = self.begin_write().await?;
        let mut created = HashSet::new();
        let mut updated = HashSet::new();
        for pair in pairs {
            let term_norm = Normalized::new(&pair.term);
            let inserted: Option<WordId> = sqlx::query_scalar(
                "INSERT INTO words (collection_id, term, term_norm) VALUES (?, ?, ?) \
                 ON CONFLICT(collection_id, term_norm) DO NOTHING RETURNING id",
            )
            .bind(collection_id)
            .bind(&pair.term)
            .bind(term_norm.as_str())
            .fetch_optional(&mut *transaction)
            .await?;
            let word_id = match inserted {
                Some(id) => {
                    created.insert(id);
                    id
                }
                None => {
                    sqlx::query_scalar(
                        "SELECT id FROM words WHERE collection_id = ? AND term_norm = ?",
                    )
                    .bind(collection_id)
                    .bind(term_norm.as_str())
                    .fetch_one(&mut *transaction)
                    .await?
                }
            };
            let added = sqlx::query(
                "INSERT INTO translations (word_id, text, text_norm) VALUES (?, ?, ?) \
                 ON CONFLICT(word_id, text_norm) DO NOTHING",
            )
            .bind(word_id)
            .bind(&pair.translation)
            .bind(Normalized::new(&pair.translation).as_str())
            .execute(&mut *transaction)
            .await?
            .rows_affected()
                > 0;
            if added && !created.contains(&word_id) {
                updated.insert(word_id);
            }
        }
        transaction.commit().await?;
        Ok(ImportSummary {
            added: created.len() as u32,
            updated: updated.len() as u32,
        })
    }

    pub async fn count_words(&self, collection_id: CollectionId) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT COUNT(*) FROM words WHERE collection_id = ?")
            .bind(collection_id)
            .fetch_one(&self.pool)
            .await
    }

    /// Words ordered by normalised term, so paging is stable across spelling
    /// case.
    pub async fn list_words(
        &self,
        collection_id: CollectionId,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<WordView>, sqlx::Error> {
        let rows: Vec<(WordId, String, Option<String>)> = sqlx::query_as(
            "SELECT w.id, w.term, t.text \
             FROM (SELECT id, term, term_norm FROM words WHERE collection_id = ? \
                   ORDER BY term_norm, id LIMIT ? OFFSET ?) w \
             LEFT JOIN translations t ON t.word_id = w.id \
             ORDER BY w.term_norm, w.id, t.id",
        )
        .bind(collection_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let mut words: Vec<WordView> = Vec::new();
        for (id, term, text) in rows {
            if words.last().is_none_or(|word| word.id != id) {
                words.push(WordView {
                    id,
                    term,
                    translations: Vec::new(),
                });
            }
            if let (Some(word), Some(text)) = (words.last_mut(), text) {
                word.translations.push(text);
            }
        }
        Ok(words)
    }

    /// `term` is normalised before the lookup.
    pub async fn find_word(
        &self,
        collection_id: CollectionId,
        term: &str,
    ) -> Result<Option<WordView>, sqlx::Error> {
        let word: Option<(WordId, String)> =
            sqlx::query_as("SELECT id, term FROM words WHERE collection_id = ? AND term_norm = ?")
                .bind(collection_id)
                .bind(Normalized::new(term).as_str())
                .fetch_optional(&self.pool)
                .await?;
        let Some((id, term)) = word else {
            return Ok(None);
        };
        let translations = Self::word_translations(&self.pool, id).await?;
        Ok(Some(WordView {
            id,
            term,
            translations,
        }))
    }

    /// Translations of the word in insertion order.
    pub(super) async fn word_translations<'e>(
        executor: impl SqliteExecutor<'e>,
        word_id: WordId,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT text FROM translations WHERE word_id = ? ORDER BY id")
            .bind(word_id)
            .fetch_all(executor)
            .await
    }

    /// Deletes the word for all members. Returns `None` if the word does not
    /// exist or the user is not a member of its collection. Owners of cancelled
    /// open quizzes get their next quiz scheduled one period from `now`.
    pub async fn remove_word(
        &self,
        user_id: UserId,
        word_id: WordId,
        now: Timestamp,
    ) -> Result<Option<Vec<CancelledQuiz>>, sqlx::Error> {
        let mut transaction = self.begin_write().await?;
        let collection_id: Option<CollectionId> =
            sqlx::query_scalar("SELECT collection_id FROM words WHERE id = ?")
                .bind(word_id)
                .fetch_optional(&mut *transaction)
                .await?;
        let Some(collection_id) = collection_id else {
            return Ok(None);
        };
        if !Self::is_member(&mut *transaction, user_id, collection_id).await? {
            return Ok(None);
        }
        let cancelled =
            Self::cancel_open_quizzes(&mut transaction, QuizScope::Word(word_id), now).await?;
        sqlx::query("DELETE FROM words WHERE id = ?")
            .bind(word_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(Some(cancelled))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ids::{CollectionId, MessageId},
        import::ParsedPair,
        period::Period,
        storage::{CancelledQuiz, Storage, testing::*},
    };

    fn pair(term: &str, summary: &str) -> ParsedPair {
        ParsedPair {
            term: term.into(),
            translation: summary.into(),
        }
    }

    async fn setup_collection() -> (Storage, CollectionId) {
        let storage = Storage::in_memory().await;
        storage
            .ensure_user(uid(1), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        let code = "K7QM-3XPA".parse().unwrap();
        let collection = storage
            .create_collection(uid(1), "A", &code, ts(0))
            .await
            .unwrap()
            .unwrap()
            .id;
        (storage, collection)
    }

    #[tokio::test]
    async fn import_merges_case_insensitively() {
        let (storage, collection) = setup_collection().await;
        let summary = storage
            .import_pairs(
                collection,
                &[
                    pair("Run", "бежать"),
                    pair("run", "управлять"),
                    pair("RUN", "Бежать"),
                ],
            )
            .await
            .unwrap();
        assert_eq!((summary.added, summary.updated), (1, 0));
        let word = storage.find_word(collection, "run").await.unwrap().unwrap();
        assert_eq!(word.term, "Run");
        assert_eq!(word.translations, ["бежать", "управлять"]);
    }

    #[tokio::test]
    async fn reimport_counts_only_real_updates() {
        let (storage, collection) = setup_collection().await;
        storage
            .import_pairs(collection, &[pair("run", "бежать")])
            .await
            .unwrap();
        let same = storage
            .import_pairs(collection, &[pair("run", "бежать")])
            .await
            .unwrap();
        assert_eq!((same.added, same.updated), (0, 0));
        let new = storage
            .import_pairs(collection, &[pair("run", "управлять")])
            .await
            .unwrap();
        assert_eq!((new.added, new.updated), (0, 1));
    }

    #[tokio::test]
    async fn same_term_in_two_collections_is_separate() {
        let (storage, first_collection) = setup_collection().await;
        let code = "AAAA-BBBB".parse().unwrap();
        let second_collection = storage
            .create_collection(uid(1), "B", &code, ts(0))
            .await
            .unwrap()
            .unwrap()
            .id;
        storage
            .import_pairs(first_collection, &[pair("run", "бежать")])
            .await
            .unwrap();
        storage
            .import_pairs(second_collection, &[pair("run", "управлять")])
            .await
            .unwrap();
        assert_eq!(
            storage
                .find_word(first_collection, "run")
                .await
                .unwrap()
                .unwrap()
                .translations,
            ["бежать"]
        );
    }

    #[tokio::test]
    async fn list_and_count_are_ordered_and_paged() {
        let (storage, collection) = setup_collection().await;
        storage
            .import_pairs(
                collection,
                &[pair("b", "2"), pair("A", "1"), pair("c", "3")],
            )
            .await
            .unwrap();
        assert_eq!(storage.count_words(collection).await.unwrap(), 3);
        let page = storage.list_words(collection, 1, 2).await.unwrap();
        let terms: Vec<_> = page.iter().map(|word| word.term.as_str()).collect();
        assert_eq!(terms, ["b", "c"]);
        assert_eq!(page[0].translations, ["2"]);
    }

    #[tokio::test]
    async fn remove_requires_membership() {
        let (storage, collection) = setup_collection().await;
        storage
            .ensure_user(uid(2), Period::from_secs(60).unwrap(), ts(0))
            .await
            .unwrap();
        storage
            .import_pairs(collection, &[pair("run", "бежать")])
            .await
            .unwrap();
        let id = storage
            .find_word(collection, "run")
            .await
            .unwrap()
            .unwrap()
            .id;
        assert!(
            storage
                .remove_word(uid(2), id, ts(0))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .remove_word(uid(1), id, ts(0))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            storage
                .find_word(collection, "run")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn remove_cancels_open_quizzes_and_reschedules_owners() {
        let (storage, collection) = setup_collection().await;
        storage
            .ensure_user(uid(2), Period::from_secs(100).unwrap(), ts(0))
            .await
            .unwrap();
        storage
            .join_collection(uid(2), &"K7QM-3XPA".parse().unwrap(), ts(0))
            .await
            .unwrap();
        storage
            .import_pairs(collection, &[pair("run", "бежать")])
            .await
            .unwrap();
        let id = storage
            .find_word(collection, "run")
            .await
            .unwrap()
            .unwrap()
            .id;
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, message_id, sent_at) \
             VALUES (2, ?, '[]', 0, 77, 0)",
        )
        .bind(id)
        .execute(&storage.pool)
        .await
        .unwrap();
        let cancelled = storage
            .remove_word(uid(1), id, ts(1000))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            cancelled,
            [CancelledQuiz {
                user_id: uid(2),
                message_id: Some(MessageId(77))
            }]
        );
        assert_eq!(
            storage
                .ensure_user(uid(2), Period::from_secs(5).unwrap(), ts(0))
                .await
                .unwrap()
                .next_quiz_at,
            ts(1100)
        );
        assert_eq!(
            storage
                .ensure_user(uid(1), Period::from_secs(5).unwrap(), ts(0))
                .await
                .unwrap()
                .next_quiz_at,
            ts(0)
        );
    }
}
