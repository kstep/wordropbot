use std::collections::HashMap;

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{CancelledQuiz, Storage, decode_err};
use crate::config::SelectionConfig;
use crate::selection::{AnswerRecord, error_rate};

const HARDEST_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub word_id: i64,
    pub collection_id: i64,
    pub term: String,
    pub translations: Vec<String>,
    pub answers: Vec<AnswerRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenQuiz {
    pub id: i64,
    pub word_id: i64,
    pub term: String,
    pub options: Vec<String>,
    pub correct_index: usize,
    pub message_id: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerOutcome {
    Answered {
        is_correct: bool,
        term: String,
        translations: Vec<String>,
        chosen: String,
    },
    /// No such open quiz for this user, or the choice is out of range.
    Closed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionStats {
    pub word_count: i64,
    pub answered: i64,
    pub correct: i64,
    pub hardest: Vec<(String, f64)>,
}

#[derive(Debug, thiserror::Error)]
#[error("stored quiz index {0} is out of range")]
struct BadStoredIndex(i64);

fn parse_options(raw: &str) -> Result<Vec<String>, sqlx::Error> {
    serde_json::from_str(raw).map_err(decode_err)
}

impl<'r> sqlx::FromRow<'r, SqliteRow> for OpenQuiz {
    fn from_row(row: &'r SqliteRow) -> Result<OpenQuiz, sqlx::Error> {
        let index: i64 = row.try_get("correct_index")?;
        let options: String = row.try_get("options")?;
        Ok(OpenQuiz {
            id: row.try_get("id")?,
            word_id: row.try_get("word_id")?,
            term: row.try_get("term")?,
            options: parse_options(&options)?,
            correct_index: usize::try_from(index).map_err(|_| decode_err(BadStoredIndex(index)))?,
            message_id: row.try_get("message_id")?,
        })
    }
}

/// A candidate together with the normalised term used for tie-breaking.
struct Loaded {
    candidate: Candidate,
    term_norm: String,
}

impl Storage {
    /// Words of all the user's current collections, each with its translations and
    /// the user's own answers. Reads each table once instead of once per word.
    pub async fn candidates(&self, user_id: i64) -> Result<Vec<Candidate>, sqlx::Error> {
        let loaded = self.load_candidates(user_id, None).await?;
        Ok(loaded.into_iter().map(|l| l.candidate).collect())
    }

    async fn load_candidates(
        &self,
        user_id: i64,
        collection_id: Option<i64>,
    ) -> Result<Vec<Loaded>, sqlx::Error> {
        let words: Vec<(i64, i64, String, String)> = sqlx::query_as(
            "SELECT w.id, w.collection_id, w.term, w.term_norm FROM words w \
             JOIN memberships m ON m.collection_id = w.collection_id AND m.user_id = ?1 \
             WHERE (?2 IS NULL OR w.collection_id = ?2) ORDER BY w.id",
        )
        .bind(user_id)
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await?;
        let mut loaded: Vec<Loaded> = Vec::with_capacity(words.len());
        let mut by_word: HashMap<i64, usize> = HashMap::with_capacity(words.len());
        for (word_id, collection_id, term, term_norm) in words {
            by_word.insert(word_id, loaded.len());
            loaded.push(Loaded {
                candidate: Candidate {
                    word_id,
                    collection_id,
                    term,
                    translations: Vec::new(),
                    answers: Vec::new(),
                },
                term_norm,
            });
        }

        let translations: Vec<(i64, String)> = sqlx::query_as(
            "SELECT t.word_id, t.text FROM translations t \
             JOIN words w ON w.id = t.word_id \
             JOIN memberships m ON m.collection_id = w.collection_id AND m.user_id = ?1 \
             WHERE (?2 IS NULL OR w.collection_id = ?2) ORDER BY t.id",
        )
        .bind(user_id)
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await?;
        for (word_id, text) in translations {
            if let Some(&i) = by_word.get(&word_id) {
                loaded[i].candidate.translations.push(text);
            }
        }

        let answers: Vec<(i64, i64, bool)> = sqlx::query_as(
            "SELECT q.word_id, q.answered_at, q.is_correct FROM quizzes q \
             JOIN words w ON w.id = q.word_id \
             JOIN memberships m ON m.collection_id = w.collection_id AND m.user_id = ?1 \
             WHERE q.user_id = ?1 AND q.answered_at IS NOT NULL \
             AND (?2 IS NULL OR w.collection_id = ?2) ORDER BY q.id",
        )
        .bind(user_id)
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await?;
        for (word_id, answered_at, is_correct) in answers {
            if let Some(&i) = by_word.get(&word_id) {
                loaded[i].candidate.answers.push(AnswerRecord {
                    answered_at,
                    is_correct,
                });
            }
        }
        Ok(loaded)
    }

    /// Returns `None` if the user already has an open quiz.
    pub async fn create_quiz(
        &self,
        user_id: i64,
        word_id: i64,
        options: &[String],
        correct_index: usize,
        now: i64,
    ) -> Result<Option<i64>, sqlx::Error> {
        let options = serde_json::to_string(options).map_err(decode_err)?;
        sqlx::query_scalar(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, sent_at) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(user_id)
        .bind(word_id)
        .bind(options)
        .bind(correct_index as i64)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn set_quiz_message(&self, quiz_id: i64, message_id: i32) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE quizzes SET message_id = ? WHERE id = ?")
            .bind(message_id)
            .bind(quiz_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_quiz(&self, quiz_id: i64) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM quizzes WHERE id = ?")
            .bind(quiz_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn open_quiz(&self, user_id: i64) -> Result<Option<OpenQuiz>, sqlx::Error> {
        sqlx::query_as(
            "SELECT q.id, q.word_id, w.term, q.options, q.correct_index, q.message_id \
             FROM quizzes q JOIN words w ON w.id = q.word_id \
             WHERE q.user_id = ? AND q.answered_at IS NULL",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Records the answer once and schedules the next quiz one period after `now`.
    pub async fn answer_quiz(
        &self,
        quiz_id: i64,
        user_id: i64,
        chosen_index: usize,
        now: i64,
    ) -> Result<AnswerOutcome, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let row: Option<(i64, String, i64)> = sqlx::query_as(
            "SELECT word_id, options, correct_index FROM quizzes \
             WHERE id = ? AND user_id = ? AND answered_at IS NULL",
        )
        .bind(quiz_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((word_id, options, correct_index)) = row else {
            return Ok(AnswerOutcome::Closed);
        };
        let mut options = parse_options(&options)?;
        if chosen_index >= options.len() {
            return Ok(AnswerOutcome::Closed);
        }
        let is_correct = chosen_index as i64 == correct_index;
        let updated = sqlx::query(
            "UPDATE quizzes SET answered_at = ?, chosen_index = ?, is_correct = ? \
             WHERE id = ? AND user_id = ? AND answered_at IS NULL",
        )
        .bind(now)
        .bind(chosen_index as i64)
        .bind(is_correct)
        .bind(quiz_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Ok(AnswerOutcome::Closed);
        }
        sqlx::query("UPDATE users SET next_quiz_at = ? + period_secs WHERE id = ?")
            .bind(now)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let term: String = sqlx::query_scalar("SELECT term FROM words WHERE id = ?")
            .bind(word_id)
            .fetch_one(&mut *tx)
            .await?;
        let translations: Vec<String> =
            sqlx::query_scalar("SELECT text FROM translations WHERE word_id = ? ORDER BY id")
                .bind(word_id)
                .fetch_all(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(AnswerOutcome::Answered {
            is_correct,
            term,
            translations,
            chosen: options.swap_remove(chosen_index),
        })
    }

    /// Cancels the open quiz without recording an answer and reschedules the user.
    pub async fn skip_quiz(
        &self,
        user_id: i64,
        now: i64,
    ) -> Result<Option<CancelledQuiz>, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let deleted: Option<(i64, Option<i32>)> = sqlx::query_as(
            "DELETE FROM quizzes WHERE user_id = ? AND answered_at IS NULL \
             RETURNING user_id, message_id",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((user_id, message_id)) = deleted else {
            return Ok(None);
        };
        sqlx::query("UPDATE users SET next_quiz_at = ? + period_secs WHERE id = ?")
            .bind(now)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(CancelledQuiz {
            user_id,
            message_id,
        }))
    }

    /// Drops quizzes that were created but never sent, e.g. after a crash.
    pub async fn delete_unsent_quizzes(&self) -> Result<u64, sqlx::Error> {
        let result =
            sqlx::query("DELETE FROM quizzes WHERE answered_at IS NULL AND message_id IS NULL")
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected())
    }

    pub async fn collection_stats(
        &self,
        user_id: i64,
        collection_id: i64,
        now: i64,
        cfg: &SelectionConfig,
    ) -> Result<CollectionStats, sqlx::Error> {
        let word_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM words WHERE collection_id = ?")
                .bind(collection_id)
                .fetch_one(&self.pool)
                .await?;
        let loaded = self.load_candidates(user_id, Some(collection_id)).await?;
        let (mut answered, mut correct) = (0, 0);
        let mut ranked: Vec<(f64, String, String)> = Vec::new();
        for l in loaded {
            let answers = &l.candidate.answers;
            answered += answers.len() as i64;
            correct += answers.iter().filter(|a| a.is_correct).count() as i64;
            if !answers.is_empty() {
                ranked.push((error_rate(answers, now, cfg), l.term_norm, l.candidate.term));
            }
        }
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        ranked.truncate(HARDEST_LIMIT);
        Ok(CollectionStats {
            word_count,
            answered,
            correct,
            hardest: ranked.into_iter().map(|(p, _, term)| (term, p)).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::Code;
    use crate::config::SelectionConfig;
    use crate::import::ParsedPair;
    use crate::period::Period;
    use crate::storage::Storage;

    const PERIOD: i64 = 600;

    struct Fixture {
        s: Storage,
        word: i64,
        collection: i64,
        code: Code,
        opts: Vec<String>,
    }

    fn pair(t: &str, r: &str) -> ParsedPair {
        ParsedPair {
            term: t.into(),
            translation: r.into(),
        }
    }

    fn cfg() -> SelectionConfig {
        SelectionConfig {
            half_life_days: 14.0,
            prior_error: 0.5,
            prior_strength: 1.0,
            min_weight: 0.05,
        }
    }

    async fn fixture() -> Fixture {
        let s = Storage::in_memory().await;
        let period = Period::from_secs(PERIOD as u64).unwrap();
        s.ensure_user(1, period, 0).await.unwrap();
        s.ensure_user(2, period, 0).await.unwrap();
        let code: Code = "K7QM-3XPA".parse().unwrap();
        let collection = s
            .create_collection(1, "A", &code, 0)
            .await
            .unwrap()
            .unwrap()
            .id;
        s.join_collection(2, &code, 0).await.unwrap().unwrap();
        s.import_pairs(
            collection,
            &[pair("run", "бежать"), pair("run", "управлять")],
        )
        .await
        .unwrap();
        let word = s.find_word(collection, "run").await.unwrap().unwrap().id;
        let opts = ["бежать", "груша", "стол"].map(String::from).to_vec();
        Fixture {
            s,
            word,
            collection,
            code,
            opts,
        }
    }

    #[tokio::test]
    async fn only_one_open_quiz() {
        let f = fixture().await;
        assert!(
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            f.s.create_quiz(1, f.word, &f.opts, 0, 11)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn answer_counts_once_and_reschedules() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            f.s.answer_quiz(q, 1, 0, 100).await.unwrap(),
            AnswerOutcome::Answered {
                is_correct: true,
                ..
            }
        ));
        assert!(matches!(
            f.s.answer_quiz(q, 1, 1, 101).await.unwrap(),
            AnswerOutcome::Closed
        ));
        assert_eq!(
            f.s.get_user(1).await.unwrap().unwrap().next_quiz_at,
            100 + PERIOD
        );
        let c = f.s.candidates(1).await.unwrap();
        assert_eq!(
            c.iter()
                .find(|c| c.word_id == f.word)
                .unwrap()
                .answers
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn answer_reports_word_and_choice() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        let AnswerOutcome::Answered {
            is_correct,
            term,
            translations,
            chosen,
        } = f.s.answer_quiz(q, 1, 1, 100).await.unwrap()
        else {
            panic!("quiz must be open");
        };
        assert!(!is_correct);
        assert_eq!(term, "run");
        assert_eq!(translations, ["бежать", "управлять"]);
        assert_eq!(chosen, "груша");
    }

    #[tokio::test]
    async fn stranger_cannot_answer() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            f.s.answer_quiz(q, 2, 0, 100).await.unwrap(),
            AnswerOutcome::Closed
        ));
        assert!(f.s.open_quiz(1).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn out_of_range_choice_is_rejected() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            f.s.answer_quiz(q, 1, 99, 100).await.unwrap(),
            AnswerOutcome::Closed
        ));
        assert!(f.s.open_quiz(1).await.unwrap().is_some());
        assert_eq!(f.s.get_user(1).await.unwrap().unwrap().next_quiz_at, 0);
    }

    #[tokio::test]
    async fn skip_leaves_no_history() {
        let f = fixture().await;
        f.s.create_quiz(1, f.word, &f.opts, 0, 10)
            .await
            .unwrap()
            .unwrap();
        assert!(f.s.skip_quiz(1, 100).await.unwrap().is_some());
        assert!(f.s.skip_quiz(1, 100).await.unwrap().is_none());
        assert_eq!(
            f.s.get_user(1).await.unwrap().unwrap().next_quiz_at,
            100 + PERIOD
        );
        assert!(
            f.s.candidates(1)
                .await
                .unwrap()
                .iter()
                .all(|c| c.answers.is_empty())
        );
    }

    #[tokio::test]
    async fn skip_without_quiz_changes_nothing() {
        let f = fixture().await;
        assert!(f.s.skip_quiz(1, 100).await.unwrap().is_none());
        assert_eq!(f.s.get_user(1).await.unwrap().unwrap().next_quiz_at, 0);
    }

    #[tokio::test]
    async fn removing_word_cancels_other_members_quiz() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(2, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.remove_word(1, f.word, 100).await.unwrap().unwrap();
        assert!(f.s.open_quiz(2).await.unwrap().is_none());
        assert!(matches!(
            f.s.answer_quiz(q, 2, 0, 101).await.unwrap(),
            AnswerOutcome::Closed
        ));
        assert_eq!(
            f.s.get_user(2).await.unwrap().unwrap().next_quiz_at,
            100 + PERIOD
        );
    }

    #[tokio::test]
    async fn leaving_keeps_history_and_cancels_open_quiz() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(2, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.answer_quiz(q, 2, 0, 20).await.unwrap();
        f.s.create_quiz(2, f.word, &f.opts, 0, 30)
            .await
            .unwrap()
            .unwrap();
        f.s.leave_collection(2, f.collection, 40)
            .await
            .unwrap()
            .unwrap();
        assert!(f.s.open_quiz(2).await.unwrap().is_none());
        assert!(f.s.candidates(2).await.unwrap().is_empty());
        f.s.join_collection(2, &f.code, 50).await.unwrap().unwrap();
        let c = f.s.candidates(2).await.unwrap();
        assert_eq!(
            c.iter()
                .find(|c| c.word_id == f.word)
                .unwrap()
                .answers
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn startup_cleanup_removes_only_unsent() {
        let f = fixture().await;
        let sent =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.set_quiz_message(sent, 77).await.unwrap();
        f.s.create_quiz(2, f.word, &f.opts, 0, 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f.s.delete_unsent_quizzes().await.unwrap(), 1);
        assert!(f.s.open_quiz(1).await.unwrap().is_some());
        assert!(f.s.open_quiz(2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn open_quiz_round_trips_options_and_message() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 2, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.set_quiz_message(q, 5).await.unwrap();
        let open = f.s.open_quiz(1).await.unwrap().unwrap();
        assert_eq!(open.id, q);
        assert_eq!(open.term, "run");
        assert_eq!(open.options, f.opts);
        assert_eq!(open.correct_index, 2);
        assert_eq!(open.message_id, Some(5));
        f.s.delete_quiz(q).await.unwrap();
        assert!(f.s.open_quiz(1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn malformed_options_are_a_decode_error() {
        let f = fixture().await;
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, sent_at) \
             VALUES (1, ?, 'not json', 0, 0)",
        )
        .bind(f.word)
        .execute(&f.s.pool)
        .await
        .unwrap();
        assert!(matches!(
            f.s.open_quiz(1).await,
            Err(sqlx::Error::Decode(_))
        ));
    }

    #[tokio::test]
    async fn candidates_include_unanswered_words_with_ordered_translations() {
        let f = fixture().await;
        let c = f.s.candidates(1).await.unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].collection_id, f.collection);
        assert_eq!(c[0].term, "run");
        assert_eq!(c[0].translations, ["бежать", "управлять"]);
        assert!(c[0].answers.is_empty());
    }

    #[tokio::test]
    async fn candidates_hold_only_own_answers() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(2, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.answer_quiz(q, 2, 0, 20).await.unwrap();
        assert!(f.s.candidates(1).await.unwrap()[0].answers.is_empty());
        assert_eq!(f.s.candidates(2).await.unwrap()[0].answers.len(), 1);
    }

    #[tokio::test]
    async fn stats_rank_hardest_by_error_rate() {
        let f = fixture().await;
        f.s.import_pairs(
            f.collection,
            &[pair("b", "2"), pair("a", "1"), pair("unseen", "3")],
        )
        .await
        .unwrap();
        let mut now = 1000;
        for (term, choice) in [("run", 1), ("a", 1), ("b", 1)] {
            let w = f.s.find_word(f.collection, term).await.unwrap().unwrap().id;
            let q =
                f.s.create_quiz(1, w, &f.opts, 0, now)
                    .await
                    .unwrap()
                    .unwrap();
            f.s.answer_quiz(q, 1, choice, now).await.unwrap();
            now += 1;
        }
        let w =
            f.s.find_word(f.collection, "unseen")
                .await
                .unwrap()
                .unwrap()
                .id;
        let q =
            f.s.create_quiz(1, w, &f.opts, 0, now)
                .await
                .unwrap()
                .unwrap();
        f.s.answer_quiz(q, 1, 0, now).await.unwrap();
        let stats =
            f.s.collection_stats(1, f.collection, now, &cfg())
                .await
                .unwrap();
        assert_eq!(stats.word_count, 4);
        assert_eq!((stats.answered, stats.correct), (4, 1));
        let terms: Vec<_> = stats.hardest.iter().map(|(t, _)| t.as_str()).collect();
        // The one correct answer (`unseen`) gives the lowest error rate.
        assert_eq!(terms.len(), 4);
        assert_eq!(terms.last(), Some(&"unseen"));
        assert!(stats.hardest.windows(2).all(|p| p[0].1 >= p[1].1));
    }

    #[tokio::test]
    async fn stats_cap_hardest_at_ten_and_break_ties_by_term() {
        let f = fixture().await;
        let pairs: Vec<_> = (0..12).map(|i| pair(&format!("w{i:02}"), "x")).collect();
        f.s.import_pairs(f.collection, &pairs).await.unwrap();
        for i in 0..11 {
            let w =
                f.s.find_word(f.collection, &format!("w{i:02}"))
                    .await
                    .unwrap()
                    .unwrap()
                    .id;
            let q = f.s.create_quiz(1, w, &f.opts, 0, 5).await.unwrap().unwrap();
            f.s.answer_quiz(q, 1, 1, 5).await.unwrap();
        }
        let stats =
            f.s.collection_stats(1, f.collection, 5, &cfg())
                .await
                .unwrap();
        assert_eq!(stats.word_count, 13);
        assert_eq!(stats.hardest.len(), 10);
        // Equal rates, so term order decides and w10 is cut; unanswered w11 is excluded.
        assert_eq!(stats.hardest[0].0, "w00");
        assert_eq!(stats.hardest[9].0, "w09");
    }
}
