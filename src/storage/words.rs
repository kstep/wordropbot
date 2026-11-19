use std::collections::HashSet;

use sqlx::SqliteExecutor;

use super::{Storage, collections::CancelledQuiz, quizzes::QuizScope};
use crate::{
    ids::{CollectionId, UserId, WordId},
    import::{NormalizedText, ParsedPair},
    time::Timestamp,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub added: u32,
    pub updated: u32,
    /// Pairs dropped because the collection holds the maximum of words.
    pub over_word_limit: u32,
    /// Pairs dropped because their word holds the maximum of translations.
    pub over_translation_limit: u32,
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
    /// translation. Pairs over the data limits are counted and left out; a word
    /// is only ever created together with its first translation.
    pub async fn import_pairs(
        &self,
        collection_id: CollectionId,
        pairs: &[ParsedPair],
    ) -> Result<ImportSummary, sqlx::Error> {
        if pairs.is_empty() {
            return Ok(ImportSummary::default());
        }
        let mut transaction = self.begin_write().await?;
        let mut word_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM words WHERE collection_id = ?")
                .bind(collection_id)
                .fetch_one(&mut *transaction)
                .await?;
        let mut summary = ImportSummary::default();
        let mut created = HashSet::new();
        let mut updated = HashSet::new();
        for pair in pairs {
            let term_norm = NormalizedText::new(&pair.term);
            let existing: Option<WordId> = sqlx::query_scalar(
                "SELECT id FROM words WHERE collection_id = ? AND term_norm = ?",
            )
            .bind(collection_id)
            .bind(term_norm.as_str())
            .fetch_optional(&mut *transaction)
            .await?;
            let word_id = match existing {
                Some(id) => id,
                None if word_count >= self.limits.max_words_per_collection as i64 => {
                    summary.over_word_limit += 1;
                    continue;
                }
                None => {
                    let id: WordId = sqlx::query_scalar(
                        "INSERT INTO words (collection_id, term, term_norm) VALUES (?, ?, ?) \
                         RETURNING id",
                    )
                    .bind(collection_id)
                    .bind(&pair.term)
                    .bind(term_norm.as_str())
                    .fetch_one(&mut *transaction)
                    .await?;
                    word_count += 1;
                    created.insert(id);
                    id
                }
            };
            let text_norm = NormalizedText::new(&pair.translation);
            let is_known: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM translations WHERE word_id = ? AND text_norm = ?)",
            )
            .bind(word_id)
            .bind(text_norm.as_str())
            .fetch_one(&mut *transaction)
            .await?;
            if is_known {
                continue;
            }
            let translation_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM translations WHERE word_id = ?")
                    .bind(word_id)
                    .fetch_one(&mut *transaction)
                    .await?;
            if translation_count >= self.limits.max_translations_per_word as i64 {
                summary.over_translation_limit += 1;
                continue;
            }
            sqlx::query("INSERT INTO translations (word_id, text, text_norm) VALUES (?, ?, ?)")
                .bind(word_id)
                .bind(&pair.translation)
                .bind(text_norm.as_str())
                .execute(&mut *transaction)
                .await?;
            if !created.contains(&word_id) {
                updated.insert(word_id);
            }
        }
        transaction.commit().await?;
        summary.added = created.len() as u32;
        summary.updated = updated.len() as u32;
        Ok(summary)
    }

    pub async fn count_words(&self, collection_id: CollectionId) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT COUNT(*) FROM words WHERE collection_id = ?")
            .bind(collection_id)
            .fetch_one(&self.pool)
            .await
    }

    /// Words ordered by normalized term, so paging is stable across spelling
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

    /// `term` is normalized before the lookup.
    pub async fn find_word(
        &self,
        collection_id: CollectionId,
        term: &str,
    ) -> Result<Option<WordView>, sqlx::Error> {
        let word: Option<(WordId, String)> =
            sqlx::query_as("SELECT id, term FROM words WHERE collection_id = ? AND term_norm = ?")
                .bind(collection_id)
                .bind(NormalizedText::new(term).as_str())
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
        ids::CollectionId,
        period::Period,
        storage::{AnswerOutcome, CancelledQuiz, Storage, StorageLimits},
        testing::*,
    };

    const CODE: &str = "K7QM-3XPA";

    async fn setup_collection() -> (Storage, CollectionId) {
        let storage = Storage::in_memory().await;
        let collection = user_with_collection(&storage, user_id(1), CODE).await;
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

    async fn collection_with_limits(limits: StorageLimits) -> (Storage, CollectionId) {
        let storage = Storage::in_memory_with_limits(limits).await;
        let collection = user_with_collection(&storage, user_id(1), CODE).await;
        (storage, collection)
    }

    #[tokio::test]
    async fn import_keeps_what_fits_under_word_limit() {
        let (storage, collection) = collection_with_limits(StorageLimits {
            max_words_per_collection: 2,
            max_translations_per_word: 20,
            max_collections_per_user: 20,
        })
        .await;
        let summary = storage
            .import_pairs(
                collection,
                &[
                    pair("a", "1"),
                    pair("b", "2"),
                    pair("c", "3"),
                    pair("a", "4"),
                ],
            )
            .await
            .unwrap();
        assert_eq!((summary.added, summary.over_word_limit), (2, 1));
        assert_eq!(storage.count_words(collection).await.unwrap(), 2);
        let word = storage.find_word(collection, "a").await.unwrap().unwrap();
        assert_eq!(word.translations, ["1", "4"]);
        let next = storage
            .import_pairs(collection, &[pair("d", "5"), pair("b", "6")])
            .await
            .unwrap();
        assert_eq!((next.updated, next.over_word_limit), (1, 1));
    }

    #[tokio::test]
    async fn import_keeps_what_fits_under_translation_limit() {
        let (storage, collection) = collection_with_limits(StorageLimits {
            max_words_per_collection: 5000,
            max_translations_per_word: 2,
            max_collections_per_user: 20,
        })
        .await;
        let summary = storage
            .import_pairs(
                collection,
                &[
                    pair("run", "бежать"),
                    pair("run", "управлять"),
                    pair("run", "мчаться"),
                    pair("run", "Бежать"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(summary.over_translation_limit, 1);
        let word = storage.find_word(collection, "run").await.unwrap().unwrap();
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
            .create_collection(user_id(1), "B", &code, timestamp(0))
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
        add_user(&storage, user_id(2)).await;
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
                .remove_word(user_id(2), id, timestamp(0))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .remove_word(user_id(1), id, timestamp(0))
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
            .ensure_user(user_id(2), Period::from_secs(100).unwrap(), timestamp(0))
            .await
            .unwrap();
        storage
            .join_collection(user_id(2), &CODE.parse().unwrap(), timestamp(0))
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
        let quiz = storage
            .create_quiz(user_id(2), id, &["бежать".into()], 0, timestamp(0))
            .await
            .unwrap()
            .unwrap();
        storage
            .set_quiz_message(quiz, message_id(77))
            .await
            .unwrap();
        let cancelled = storage
            .remove_word(user_id(1), id, timestamp(1000))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            cancelled,
            [CancelledQuiz {
                user_id: user_id(2),
                message_id: Some(message_id(77))
            }]
        );
        assert!(storage.open_quiz(user_id(2)).await.unwrap().is_none());
        assert_eq!(
            storage
                .answer_quiz(quiz, user_id(2), 0, timestamp(1001))
                .await
                .unwrap(),
            AnswerOutcome::Closed
        );
        assert_eq!(next_quiz_at(&storage, user_id(2)).await, timestamp(1100));
        assert_eq!(next_quiz_at(&storage, user_id(1)).await, timestamp(0));
    }
}
