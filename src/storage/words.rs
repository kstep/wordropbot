use std::collections::HashSet;

use super::Storage;
use super::collections::CancelledQuiz;
use crate::import::{ParsedPair, normalize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportSummary {
    pub added: u32,
    pub updated: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordView {
    pub id: i64,
    pub term: String,
    pub translations: Vec<String>,
}

impl Storage {
    /// Merges the pairs into the collection in one transaction. A word counts as
    /// `updated` only if it existed before this import and gained a translation.
    pub async fn import_pairs(
        &self,
        collection_id: i64,
        pairs: &[ParsedPair],
    ) -> Result<ImportSummary, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let mut created = HashSet::new();
        let mut updated = HashSet::new();
        for pair in pairs {
            let inserted: Option<i64> = sqlx::query_scalar(
                "INSERT INTO words (collection_id, term, term_norm) VALUES (?, ?, ?) \
                 ON CONFLICT(collection_id, term_norm) DO NOTHING RETURNING id",
            )
            .bind(collection_id)
            .bind(&pair.term)
            .bind(normalize(&pair.term))
            .fetch_optional(&mut *tx)
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
                    .bind(normalize(&pair.term))
                    .fetch_one(&mut *tx)
                    .await?
                }
            };
            let added = sqlx::query(
                "INSERT INTO translations (word_id, text, text_norm) VALUES (?, ?, ?) \
                 ON CONFLICT(word_id, text_norm) DO NOTHING",
            )
            .bind(word_id)
            .bind(&pair.translation)
            .bind(normalize(&pair.translation))
            .execute(&mut *tx)
            .await?
            .rows_affected()
                > 0;
            if added && !created.contains(&word_id) {
                updated.insert(word_id);
            }
        }
        tx.commit().await?;
        Ok(ImportSummary {
            added: created.len() as u32,
            updated: updated.len() as u32,
        })
    }

    pub async fn count_words(&self, collection_id: i64) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT COUNT(*) FROM words WHERE collection_id = ?")
            .bind(collection_id)
            .fetch_one(&self.pool)
            .await
    }

    /// Words ordered by normalised term, so paging is stable across spelling case.
    pub async fn list_words(
        &self,
        collection_id: i64,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<WordView>, sqlx::Error> {
        let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(
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
            if words.last().is_none_or(|w| w.id != id) {
                words.push(WordView {
                    id,
                    term,
                    translations: Vec::new(),
                });
            }
            if let (Some(w), Some(text)) = (words.last_mut(), text) {
                w.translations.push(text);
            }
        }
        Ok(words)
    }

    /// `term` is normalised before the lookup.
    pub async fn find_word(
        &self,
        collection_id: i64,
        term: &str,
    ) -> Result<Option<WordView>, sqlx::Error> {
        let word: Option<(i64, String)> =
            sqlx::query_as("SELECT id, term FROM words WHERE collection_id = ? AND term_norm = ?")
                .bind(collection_id)
                .bind(normalize(term))
                .fetch_optional(&self.pool)
                .await?;
        let Some((id, term)) = word else {
            return Ok(None);
        };
        let translations =
            sqlx::query_scalar("SELECT text FROM translations WHERE word_id = ? ORDER BY id")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;
        Ok(Some(WordView {
            id,
            term,
            translations,
        }))
    }

    /// Deletes the word for all members. Returns `None` if the word does not exist or
    /// the user is not a member of its collection. Owners of cancelled open quizzes
    /// get their next quiz scheduled one period from `now`.
    pub async fn remove_word(
        &self,
        user_id: i64,
        word_id: i64,
        now: i64,
    ) -> Result<Option<Vec<CancelledQuiz>>, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let allowed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM words w \
             JOIN memberships m ON m.collection_id = w.collection_id \
             WHERE w.id = ? AND m.user_id = ?)",
        )
        .bind(word_id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if !allowed {
            return Ok(None);
        }
        let open: Vec<(i64, Option<i32>)> = sqlx::query_as(
            "SELECT user_id, message_id FROM quizzes WHERE word_id = ? AND answered_at IS NULL",
        )
        .bind(word_id)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE users SET next_quiz_at = ? + period_secs WHERE id IN \
             (SELECT user_id FROM quizzes WHERE word_id = ? AND answered_at IS NULL)",
        )
        .bind(now)
        .bind(word_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM words WHERE id = ?")
            .bind(word_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(
            open.into_iter()
                .map(|(user_id, message_id)| CancelledQuiz {
                    user_id,
                    message_id,
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::import::ParsedPair;
    use crate::period::Period;
    use crate::storage::Storage;

    fn pair(t: &str, r: &str) -> ParsedPair {
        ParsedPair {
            term: t.into(),
            translation: r.into(),
        }
    }

    async fn setup_collection() -> (Storage, i64) {
        let s = Storage::in_memory().await;
        s.ensure_user(1, Period::from_secs(60).unwrap(), 0)
            .await
            .unwrap();
        let code = "K7QM-3XPA".parse().unwrap();
        let c = s
            .create_collection(1, "A", &code, 0)
            .await
            .unwrap()
            .unwrap()
            .id;
        (s, c)
    }

    #[tokio::test]
    async fn import_merges_case_insensitively() {
        let (s, c) = setup_collection().await;
        let r = s
            .import_pairs(
                c,
                &[
                    pair("Run", "бежать"),
                    pair("run", "управлять"),
                    pair("RUN", "Бежать"),
                ],
            )
            .await
            .unwrap();
        assert_eq!((r.added, r.updated), (1, 0));
        let w = s.find_word(c, "run").await.unwrap().unwrap();
        assert_eq!(w.term, "Run");
        assert_eq!(w.translations, ["бежать", "управлять"]);
    }

    #[tokio::test]
    async fn reimport_counts_only_real_updates() {
        let (s, c) = setup_collection().await;
        s.import_pairs(c, &[pair("run", "бежать")]).await.unwrap();
        let same = s.import_pairs(c, &[pair("run", "бежать")]).await.unwrap();
        assert_eq!((same.added, same.updated), (0, 0));
        let new = s
            .import_pairs(c, &[pair("run", "управлять")])
            .await
            .unwrap();
        assert_eq!((new.added, new.updated), (0, 1));
    }

    #[tokio::test]
    async fn same_term_in_two_collections_is_separate() {
        let (s, a) = setup_collection().await;
        let code = "AAAA-BBBB".parse().unwrap();
        let b = s
            .create_collection(1, "B", &code, 0)
            .await
            .unwrap()
            .unwrap()
            .id;
        s.import_pairs(a, &[pair("run", "бежать")]).await.unwrap();
        s.import_pairs(b, &[pair("run", "управлять")])
            .await
            .unwrap();
        assert_eq!(
            s.find_word(a, "run").await.unwrap().unwrap().translations,
            ["бежать"]
        );
    }

    #[tokio::test]
    async fn list_and_count_are_ordered_and_paged() {
        let (s, c) = setup_collection().await;
        s.import_pairs(c, &[pair("b", "2"), pair("A", "1"), pair("c", "3")])
            .await
            .unwrap();
        assert_eq!(s.count_words(c).await.unwrap(), 3);
        let page = s.list_words(c, 1, 2).await.unwrap();
        let terms: Vec<_> = page.iter().map(|w| w.term.as_str()).collect();
        assert_eq!(terms, ["b", "c"]);
        assert_eq!(page[0].translations, ["2"]);
    }

    #[tokio::test]
    async fn remove_requires_membership() {
        let (s, c) = setup_collection().await;
        s.ensure_user(2, Period::from_secs(60).unwrap(), 0)
            .await
            .unwrap();
        s.import_pairs(c, &[pair("run", "бежать")]).await.unwrap();
        let id = s.find_word(c, "run").await.unwrap().unwrap().id;
        assert!(s.remove_word(2, id, 0).await.unwrap().is_none());
        assert!(s.remove_word(1, id, 0).await.unwrap().is_some());
        assert!(s.find_word(c, "run").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn remove_cancels_open_quizzes_and_reschedules_owners() {
        let (s, c) = setup_collection().await;
        s.ensure_user(2, Period::from_secs(100).unwrap(), 0)
            .await
            .unwrap();
        s.join_collection(2, &"K7QM-3XPA".parse().unwrap(), 0)
            .await
            .unwrap();
        s.import_pairs(c, &[pair("run", "бежать")]).await.unwrap();
        let id = s.find_word(c, "run").await.unwrap().unwrap().id;
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, message_id, sent_at) \
             VALUES (2, ?, '[]', 0, 77, 0)",
        )
        .bind(id)
        .execute(&s.pool)
        .await
        .unwrap();
        let cancelled = s.remove_word(1, id, 1000).await.unwrap().unwrap();
        assert_eq!(
            cancelled,
            [crate::storage::CancelledQuiz {
                user_id: 2,
                message_id: Some(77)
            }]
        );
        assert_eq!(
            s.ensure_user(2, Period::from_secs(5).unwrap(), 0)
                .await
                .unwrap()
                .next_quiz_at,
            1100
        );
        assert_eq!(
            s.ensure_user(1, Period::from_secs(5).unwrap(), 0)
                .await
                .unwrap()
                .next_quiz_at,
            0
        );
    }
}
