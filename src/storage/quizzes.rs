use chrono::{DateTime, Utc};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow, types::Json};

use super::{CancelledQuiz, Storage};
use crate::{
    config::SelectionConfig,
    ids::{CollectionId, MessageId, QuizId, UserId, WordId},
    import::NormalizedText,
    selection::AnswerRecord,
};

const HARDEST_LIMIT: usize = 10;

/// Restricts `words w` to the collections of user `?1`, and to collection `?2`
/// when one is given.
macro_rules! member_words_filter {
    () => {
        "JOIN memberships m ON m.collection_id = w.collection_id AND m.user_id = ?1 \
         WHERE (?2 IS NULL OR w.collection_id = ?2) "
    };
}

/// Answers of quizzes `q`, oldest first, as a JSON array of `AnswerRecord`.
macro_rules! answers_json {
    () => {
        "json_group_array(json_object('answered_at', q.answered_at, \
         'is_correct', json(CASE WHEN q.is_correct THEN 'true' ELSE 'false' END)) \
         ORDER BY q.id)"
    };
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Candidate {
    pub word_id: WordId,
    pub collection_id: CollectionId,
    pub term: String,
    #[sqlx(json)]
    pub translations: Vec<String>,
    #[sqlx(json)]
    pub answers: Vec<AnswerRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenQuiz {
    pub id: QuizId,
    pub term: String,
    pub options: Vec<String>,
    pub correct_index: usize,
    pub message_id: Option<MessageId>,
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
    pub word_count: u32,
    pub answered: usize,
    pub correct: usize,
    pub hardest: Vec<HardWord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HardWord {
    pub term: String,
    pub error_rate: f64,
}

/// Which open quizzes a cancellation removes.
pub(super) enum QuizScope {
    /// The user's open quiz, whatever its word.
    User(UserId),
    /// The user's open quiz if its word belongs to the collection.
    UserInCollection {
        user_id: UserId,
        collection_id: CollectionId,
    },
    /// Open quizzes of every user on this word.
    Word(WordId),
}

impl QuizScope {
    fn user_id(&self) -> Option<UserId> {
        match *self {
            QuizScope::User(user_id) | QuizScope::UserInCollection { user_id, .. } => Some(user_id),
            QuizScope::Word(_) => None,
        }
    }

    fn collection_id(&self) -> Option<CollectionId> {
        match *self {
            QuizScope::UserInCollection { collection_id, .. } => Some(collection_id),
            _ => None,
        }
    }

    fn word_id(&self) -> Option<WordId> {
        match *self {
            QuizScope::Word(word_id) => Some(word_id),
            _ => None,
        }
    }
}

impl<'r> sqlx::FromRow<'r, SqliteRow> for OpenQuiz {
    fn from_row(row: &'r SqliteRow) -> Result<OpenQuiz, sqlx::Error> {
        let options: Json<Vec<String>> = row.try_get("options")?;
        let correct_index: u32 = row.try_get("correct_index")?;
        Ok(OpenQuiz {
            id: row.try_get("id")?,
            term: row.try_get("term")?,
            options: options.0,
            correct_index: correct_index as usize,
            message_id: row.try_get("message_id")?,
        })
    }
}

impl Storage {
    /// Words of all the user's current collections, each with its translations
    /// and the user's own answers. Reads each table once instead of once per
    /// word.
    pub async fn candidates(&self, user_id: UserId) -> Result<Vec<Candidate>, sqlx::Error> {
        let collection_id: Option<CollectionId> = None;
        sqlx::query_as(concat!(
            "SELECT w.id AS word_id, w.collection_id, w.term, \
                    (SELECT json_group_array(t.text ORDER BY t.id) FROM translations t \
                     WHERE t.word_id = w.id) AS translations, \
                    (SELECT ",
            answers_json!(),
            " FROM quizzes q WHERE q.word_id = w.id AND q.user_id = ?1 \
                     AND q.answered_at IS NOT NULL) AS answers \
             FROM words w ",
            member_words_filter!(),
            "ORDER BY w.id",
        ))
        .bind(user_id)
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Whether some collection of the user holds at least `options` distinct
    /// translations. A quiz needs the correct option plus `options - 1`
    /// distinct others from one collection, so this is necessary for any word
    /// to be eligible, and it is cheaper than loading the candidates.
    pub async fn has_enough_distinct_translations(
        &self,
        user_id: UserId,
        options: usize,
    ) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM memberships m \
             JOIN words w ON w.collection_id = m.collection_id \
             JOIN translations t ON t.word_id = w.id \
             WHERE m.user_id = ? GROUP BY m.collection_id \
             HAVING COUNT(DISTINCT t.text_norm) >= ?)",
        )
        .bind(user_id)
        .bind(options as u32)
        .fetch_one(&self.pool)
        .await
    }

    /// Returns `None` if the user already has an open quiz or is no longer a
    /// member of the word's collection.
    pub async fn create_quiz(
        &self,
        user_id: UserId,
        word_id: WordId,
        options: &[String],
        correct_index: usize,
        now: DateTime<Utc>,
    ) -> Result<Option<QuizId>, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, sent_at) \
             SELECT ?1, w.id, ?3, ?4, ?5 FROM words w \
             JOIN memberships m ON m.collection_id = w.collection_id AND m.user_id = ?1 \
             WHERE w.id = ?2 ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(user_id)
        .bind(word_id)
        .bind(Json(options))
        .bind(correct_index as u32)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
    }

    /// Returns `false` if the quiz no longer exists.
    pub async fn set_quiz_message(
        &self,
        quiz_id: QuizId,
        message_id: MessageId,
    ) -> Result<bool, sqlx::Error> {
        let updated = sqlx::query("UPDATE quizzes SET message_id = ? WHERE id = ?")
            .bind(message_id)
            .bind(quiz_id)
            .execute(&self.pool)
            .await?;
        Ok(updated.rows_affected() > 0)
    }

    /// Removes a quiz that was never answered; a recorded answer is history
    /// and stays, even if the send that created the quiz reports a failure.
    pub async fn delete_quiz(&self, quiz_id: QuizId) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM quizzes WHERE id = ? AND answered_at IS NULL")
            .bind(quiz_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn open_quiz(&self, user_id: UserId) -> Result<Option<OpenQuiz>, sqlx::Error> {
        sqlx::query_as(
            "SELECT q.id, w.term, q.options, q.correct_index, q.message_id \
             FROM quizzes q JOIN words w ON w.id = q.word_id \
             WHERE q.user_id = ? AND q.answered_at IS NULL",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Records the answer once and schedules the next quiz one period after
    /// `now`.
    pub async fn answer_quiz(
        &self,
        quiz_id: QuizId,
        user_id: UserId,
        chosen_index: usize,
        now: DateTime<Utc>,
    ) -> Result<AnswerOutcome, sqlx::Error> {
        let mut transaction = self.begin_write().await?;
        let row: Option<(WordId, Json<Vec<String>>, u32)> = sqlx::query_as(
            "SELECT word_id, options, correct_index FROM quizzes \
             WHERE id = ? AND user_id = ? AND answered_at IS NULL",
        )
        .bind(quiz_id)
        .bind(user_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((word_id, options, correct_index)) = row else {
            return Ok(AnswerOutcome::Closed);
        };
        let mut options = options.0;
        if chosen_index >= options.len() {
            return Ok(AnswerOutcome::Closed);
        }
        let translations = Self::word_translations(&mut *transaction, word_id).await?;
        // Options are frozen at creation, but a member may have imported one of
        // them as a translation of this word since.
        let is_correct = chosen_index == correct_index as usize || {
            let chosen = NormalizedText::new(&options[chosen_index]);
            translations
                .iter()
                .any(|translation| NormalizedText::new(translation) == chosen)
        };
        // The write lock has kept the row open since the SELECT above.
        sqlx::query(
            "UPDATE quizzes SET answered_at = ?, chosen_index = ?, is_correct = ? \
             WHERE id = ? AND user_id = ? AND answered_at IS NULL",
        )
        .bind(now)
        .bind(chosen_index as u32)
        .bind(is_correct)
        .bind(quiz_id)
        .bind(user_id)
        .execute(&mut *transaction)
        .await?;
        Storage::reschedule(&mut transaction, user_id, now).await?;
        let term: String = sqlx::query_scalar("SELECT term FROM words WHERE id = ?")
            .bind(word_id)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(AnswerOutcome::Answered {
            is_correct,
            term,
            translations,
            chosen: options.swap_remove(chosen_index),
        })
    }

    /// Cancels the open quiz without recording an answer and reschedules the
    /// user.
    pub async fn skip_quiz(
        &self,
        user_id: UserId,
        now: DateTime<Utc>,
    ) -> Result<Option<CancelledQuiz>, sqlx::Error> {
        let mut transaction = self.begin_write().await?;
        let cancelled =
            Self::cancel_open_quizzes(&mut transaction, QuizScope::User(user_id), now).await?;
        transaction.commit().await?;
        Ok(cancelled.into_iter().next())
    }

    /// Deletes the open quizzes in scope and schedules each owner one period
    /// from `now`. The caller commits.
    pub(super) async fn cancel_open_quizzes(
        transaction: &mut SqliteConnection,
        scope: QuizScope,
        now: DateTime<Utc>,
    ) -> Result<Vec<CancelledQuiz>, sqlx::Error> {
        let cancelled: Vec<CancelledQuiz> = sqlx::query_as(
            "DELETE FROM quizzes WHERE answered_at IS NULL \
             AND (?1 IS NULL OR user_id = ?1) \
             AND (?2 IS NULL OR word_id IN (SELECT id FROM words WHERE collection_id = ?2)) \
             AND (?3 IS NULL OR word_id = ?3) \
             RETURNING user_id, message_id",
        )
        .bind(scope.user_id())
        .bind(scope.collection_id())
        .bind(scope.word_id())
        .fetch_all(&mut *transaction)
        .await?;
        for quiz in &cancelled {
            Self::reschedule(&mut *transaction, quiz.user_id, now).await?;
        }
        Ok(cancelled)
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
        user_id: UserId,
        collection_id: CollectionId,
        now: DateTime<Utc>,
        selection: &SelectionConfig,
    ) -> Result<CollectionStats, sqlx::Error> {
        let word_count = self.count_words(collection_id).await?;
        let rows: Vec<(String, String, Json<Vec<AnswerRecord>>)> = sqlx::query_as(concat!(
            "SELECT w.term, w.term_norm, ",
            answers_json!(),
            " FROM quizzes q JOIN words w ON w.id = q.word_id ",
            member_words_filter!(),
            "AND q.user_id = ?1 AND q.answered_at IS NOT NULL GROUP BY w.id",
        ))
        .bind(user_id)
        .bind(Some(collection_id))
        .fetch_all(&self.pool)
        .await?;
        let (mut answered, mut correct) = (0, 0);
        // The normalized term breaks error-rate ties.
        let mut ranked: Vec<(f64, String, String)> = Vec::with_capacity(rows.len());
        for (term, term_norm, Json(answers)) in rows {
            answered += answers.len();
            correct += answers.iter().filter(|answer| answer.is_correct).count();
            ranked.push((selection.error_rate(&answers, now), term_norm, term));
        }
        ranked.sort_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        ranked.truncate(HARDEST_LIMIT);
        Ok(CollectionStats {
            word_count,
            answered,
            correct,
            hardest: ranked
                .into_iter()
                .map(|(error_rate, _, term)| HardWord { term, error_rate })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use sqlx::Error;

    use super::*;
    use crate::{code::Code, period::Period, storage::Storage, testing::*};

    const PERIOD: i64 = 600;

    struct Fixture {
        storage: Storage,
        word: WordId,
        collection: CollectionId,
        code: Code,
        options: Vec<String>,
    }

    async fn fixture() -> Fixture {
        let storage = Storage::in_memory().await;
        let period = Period::from_secs(PERIOD as u64).unwrap();
        storage
            .ensure_user(user_id(1), period, timestamp(0))
            .await
            .unwrap();
        storage
            .ensure_user(user_id(2), period, timestamp(0))
            .await
            .unwrap();
        let code: Code = "K7QM-3XPA".parse().unwrap();
        let collection = storage
            .create_collection(user_id(1), "A", &code, timestamp(0))
            .await
            .unwrap()
            .unwrap()
            .id;
        storage
            .join_collection(user_id(2), &code, timestamp(0))
            .await
            .unwrap()
            .unwrap();
        storage
            .import_pairs(
                collection,
                &[pair("run", "бежать"), pair("run", "управлять")],
            )
            .await
            .unwrap();
        let word = storage
            .find_word(collection, "run")
            .await
            .unwrap()
            .unwrap()
            .id;
        let options = ["бежать", "груша", "стол"].map(String::from).to_vec();
        Fixture {
            storage,
            word,
            collection,
            code,
            options,
        }
    }

    #[tokio::test]
    async fn only_one_open_quiz() {
        let fixture = fixture().await;
        assert!(
            fixture
                .storage
                .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(10))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            fixture
                .storage
                .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(11))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn answer_counts_once_and_reschedules() {
        let fixture = fixture().await;
        let quiz = fixture
            .storage
            .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            fixture
                .storage
                .answer_quiz(quiz, user_id(1), 0, timestamp(100))
                .await
                .unwrap(),
            AnswerOutcome::Answered {
                is_correct: true,
                ..
            }
        ));
        assert!(matches!(
            fixture
                .storage
                .answer_quiz(quiz, user_id(1), 1, timestamp(101))
                .await
                .unwrap(),
            AnswerOutcome::Closed
        ));
        assert_eq!(
            next_quiz_at(&fixture.storage, user_id(1)).await,
            timestamp(100 + PERIOD)
        );
        let candidates = fixture.storage.candidates(user_id(1)).await.unwrap();
        assert_eq!(
            candidates
                .iter()
                .find(|candidate| candidate.word_id == fixture.word)
                .unwrap()
                .answers
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn deleting_answered_quiz_keeps_recorded_answer() {
        let fixture = fixture().await;
        let quiz = fixture
            .storage
            .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        fixture
            .storage
            .answer_quiz(quiz, user_id(1), 0, timestamp(20))
            .await
            .unwrap();
        fixture.storage.delete_quiz(quiz).await.unwrap();
        let candidates = fixture.storage.candidates(user_id(1)).await.unwrap();
        assert_eq!(candidates[0].answers.len(), 1);
    }

    #[tokio::test]
    async fn translation_added_during_open_quiz_counts_as_correct() {
        let fixture = fixture().await;
        let quiz = fixture
            .storage
            .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        fixture
            .storage
            .import_pairs(fixture.collection, &[pair("run", "Груша")])
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .storage
                .answer_quiz(quiz, user_id(1), 1, timestamp(20))
                .await
                .unwrap(),
            AnswerOutcome::Answered {
                is_correct: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn quiz_is_not_created_after_leaving_collection() {
        let fixture = fixture().await;
        fixture
            .storage
            .leave_collection(user_id(2), fixture.collection, timestamp(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fixture
                .storage
                .create_quiz(user_id(2), fixture.word, &fixture.options, 0, timestamp(10))
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn answer_reports_word_and_choice() {
        let fixture = fixture().await;
        let quiz = fixture
            .storage
            .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        let AnswerOutcome::Answered {
            is_correct,
            term,
            translations,
            chosen,
        } = fixture
            .storage
            .answer_quiz(quiz, user_id(1), 1, timestamp(100))
            .await
            .unwrap()
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
        let fixture = fixture().await;
        let quiz = fixture
            .storage
            .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            fixture
                .storage
                .answer_quiz(quiz, user_id(2), 0, timestamp(100))
                .await
                .unwrap(),
            AnswerOutcome::Closed
        ));
        assert!(
            fixture
                .storage
                .open_quiz(user_id(1))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn out_of_range_choice_is_rejected() {
        let fixture = fixture().await;
        let quiz = fixture
            .storage
            .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            fixture
                .storage
                .answer_quiz(quiz, user_id(1), 99, timestamp(100))
                .await
                .unwrap(),
            AnswerOutcome::Closed
        ));
        assert!(
            fixture
                .storage
                .open_quiz(user_id(1))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            next_quiz_at(&fixture.storage, user_id(1)).await,
            timestamp(0)
        );
    }

    #[tokio::test]
    async fn skip_leaves_no_history() {
        let fixture = fixture().await;
        // Without an open quiz nothing is cancelled or rescheduled.
        assert!(
            fixture
                .storage
                .skip_quiz(user_id(1), timestamp(50))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            next_quiz_at(&fixture.storage, user_id(1)).await,
            timestamp(0)
        );
        fixture
            .storage
            .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        assert!(
            fixture
                .storage
                .skip_quiz(user_id(1), timestamp(100))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            fixture
                .storage
                .skip_quiz(user_id(1), timestamp(100))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            next_quiz_at(&fixture.storage, user_id(1)).await,
            timestamp(100 + PERIOD)
        );
        assert!(
            fixture
                .storage
                .candidates(user_id(1))
                .await
                .unwrap()
                .iter()
                .all(|candidate| candidate.answers.is_empty())
        );
    }

    #[tokio::test]
    async fn leaving_keeps_history_and_cancels_open_quiz() {
        let fixture = fixture().await;
        let quiz = fixture
            .storage
            .create_quiz(user_id(2), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        fixture
            .storage
            .answer_quiz(quiz, user_id(2), 0, timestamp(20))
            .await
            .unwrap();
        fixture
            .storage
            .create_quiz(user_id(2), fixture.word, &fixture.options, 0, timestamp(30))
            .await
            .unwrap()
            .unwrap();
        fixture
            .storage
            .leave_collection(user_id(2), fixture.collection, timestamp(40))
            .await
            .unwrap()
            .unwrap();
        assert!(
            fixture
                .storage
                .open_quiz(user_id(2))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .storage
                .candidates(user_id(2))
                .await
                .unwrap()
                .is_empty()
        );
        fixture
            .storage
            .join_collection(user_id(2), &fixture.code, timestamp(50))
            .await
            .unwrap()
            .unwrap();
        let candidates = fixture.storage.candidates(user_id(2)).await.unwrap();
        assert_eq!(
            candidates
                .iter()
                .find(|candidate| candidate.word_id == fixture.word)
                .unwrap()
                .answers
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn startup_cleanup_removes_only_unsent() {
        let fixture = fixture().await;
        let sent = fixture
            .storage
            .create_quiz(user_id(1), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        fixture
            .storage
            .set_quiz_message(sent, message_id(77))
            .await
            .unwrap();
        fixture
            .storage
            .create_quiz(user_id(2), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fixture.storage.delete_unsent_quizzes().await.unwrap(), 1);
        assert!(
            fixture
                .storage
                .open_quiz(user_id(1))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            fixture
                .storage
                .open_quiz(user_id(2))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn open_quiz_round_trips_options_and_message() {
        let fixture = fixture().await;
        let quiz = fixture
            .storage
            .create_quiz(user_id(1), fixture.word, &fixture.options, 2, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        fixture
            .storage
            .set_quiz_message(quiz, message_id(5))
            .await
            .unwrap();
        let open = fixture
            .storage
            .open_quiz(user_id(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open.id, quiz);
        assert_eq!(open.term, "run");
        assert_eq!(open.options, fixture.options);
        assert_eq!(open.message_id, Some(message_id(5)));
        fixture.storage.delete_quiz(quiz).await.unwrap();
        assert!(
            fixture
                .storage
                .open_quiz(user_id(1))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn stored_correct_index_decides_score() {
        let fixture = fixture().await;
        // None of these is a translation, so only the stored index can score.
        let options = ["груша", "стол", "окно"].map(String::from);
        for (chosen_index, expected) in [(2, true), (0, false)] {
            let quiz = fixture
                .storage
                .create_quiz(user_id(1), fixture.word, &options, 2, timestamp(10))
                .await
                .unwrap()
                .unwrap();
            let open = fixture
                .storage
                .open_quiz(user_id(1))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(open.correct_index, 2);
            let outcome = fixture
                .storage
                .answer_quiz(quiz, user_id(1), chosen_index, timestamp(20))
                .await
                .unwrap();
            assert!(
                matches!(outcome, AnswerOutcome::Answered { is_correct, .. } if is_correct == expected),
                "choice {chosen_index}: {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn malformed_options_are_decode_error() {
        let fixture = fixture().await;
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, sent_at) \
             VALUES (1, ?, 'not json', 0, 0)",
        )
        .bind(fixture.word)
        .execute(&fixture.storage.pool)
        .await
        .unwrap();
        assert!(matches!(
            fixture.storage.open_quiz(user_id(1)).await,
            Err(Error::ColumnDecode { .. })
        ));
    }

    #[tokio::test]
    async fn candidates_include_unanswered_words_with_ordered_translations() {
        let fixture = fixture().await;
        let candidates = fixture.storage.candidates(user_id(1)).await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].collection_id, fixture.collection);
        assert_eq!(candidates[0].term, "run");
        assert_eq!(candidates[0].translations, ["бежать", "управлять"]);
        assert!(candidates[0].answers.is_empty());
    }

    #[tokio::test]
    async fn candidates_hold_only_own_answers() {
        let fixture = fixture().await;
        let quiz = fixture
            .storage
            .create_quiz(user_id(2), fixture.word, &fixture.options, 0, timestamp(10))
            .await
            .unwrap()
            .unwrap();
        fixture
            .storage
            .answer_quiz(quiz, user_id(2), 0, timestamp(20))
            .await
            .unwrap();
        assert!(
            fixture.storage.candidates(user_id(1)).await.unwrap()[0]
                .answers
                .is_empty()
        );
        assert_eq!(
            fixture.storage.candidates(user_id(2)).await.unwrap()[0]
                .answers
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn stats_rank_hardest_by_error_rate() {
        let fixture = fixture().await;
        fixture
            .storage
            .import_pairs(
                fixture.collection,
                &[pair("b", "2"), pair("a", "1"), pair("unseen", "3")],
            )
            .await
            .unwrap();
        let mut now = timestamp(1000);
        for (term, choice) in [("run", 1), ("a", 1), ("b", 1)] {
            let word = fixture
                .storage
                .find_word(fixture.collection, term)
                .await
                .unwrap()
                .unwrap()
                .id;
            let quiz = fixture
                .storage
                .create_quiz(user_id(1), word, &fixture.options, 0, now)
                .await
                .unwrap()
                .unwrap();
            fixture
                .storage
                .answer_quiz(quiz, user_id(1), choice, now)
                .await
                .unwrap();
            now += TimeDelta::seconds(1);
        }
        let word = fixture
            .storage
            .find_word(fixture.collection, "unseen")
            .await
            .unwrap()
            .unwrap()
            .id;
        let quiz = fixture
            .storage
            .create_quiz(user_id(1), word, &fixture.options, 0, now)
            .await
            .unwrap()
            .unwrap();
        fixture
            .storage
            .answer_quiz(quiz, user_id(1), 0, now)
            .await
            .unwrap();
        let stats = fixture
            .storage
            .collection_stats(
                user_id(1),
                fixture.collection,
                now,
                &SelectionConfig::default(),
            )
            .await
            .unwrap();
        assert_eq!(stats.word_count, 4);
        assert_eq!((stats.answered, stats.correct), (4, 1));
        let terms: Vec<_> = stats
            .hardest
            .iter()
            .map(|hard| hard.term.as_str())
            .collect();
        // The one correct answer (`unseen`) gives the lowest error rate.
        assert_eq!(terms.len(), 4);
        assert_eq!(terms.last(), Some(&"unseen"));
        assert!(
            stats
                .hardest
                .windows(2)
                .all(|window| window[0].error_rate >= window[1].error_rate)
        );
    }

    #[tokio::test]
    async fn stats_cap_hardest_at_ten_and_break_ties_by_term() {
        let fixture = fixture().await;
        let pairs: Vec<_> = (0..12)
            .map(|number| pair(&format!("w{number:02}"), "x"))
            .collect();
        fixture
            .storage
            .import_pairs(fixture.collection, &pairs)
            .await
            .unwrap();
        for number in 0..11 {
            let word = fixture
                .storage
                .find_word(fixture.collection, &format!("w{number:02}"))
                .await
                .unwrap()
                .unwrap()
                .id;
            let quiz = fixture
                .storage
                .create_quiz(user_id(1), word, &fixture.options, 0, timestamp(5))
                .await
                .unwrap()
                .unwrap();
            fixture
                .storage
                .answer_quiz(quiz, user_id(1), 1, timestamp(5))
                .await
                .unwrap();
        }
        let stats = fixture
            .storage
            .collection_stats(
                user_id(1),
                fixture.collection,
                timestamp(5),
                &SelectionConfig::default(),
            )
            .await
            .unwrap();
        assert_eq!(stats.word_count, 13);
        assert_eq!(stats.hardest.len(), 10);
        // Equal rates, so term order decides and w10 is cut; unanswered w11 is
        // excluded.
        assert_eq!(stats.hardest[0].term, "w00");
        assert_eq!(stats.hardest[9].term, "w09");
    }

    #[tokio::test]
    async fn too_few_distinct_translations_fail_precheck() {
        let fixture = fixture().await;
        // Two distinct translations so far; repeats of one add nothing.
        fixture
            .storage
            .import_pairs(
                fixture.collection,
                &[pair("a", "стол"), pair("b", "Стол"), pair("c", " стол ")],
            )
            .await
            .unwrap();
        assert!(
            !fixture
                .storage
                .has_enough_distinct_translations(user_id(1), 4)
                .await
                .unwrap()
        );
        assert!(
            fixture
                .storage
                .has_enough_distinct_translations(user_id(1), 3)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn precheck_counts_each_collection_separately() {
        let fixture = fixture().await;
        let other = fixture
            .storage
            .create_collection(user_id(1), "B", &"AAAA-BBBB".parse().unwrap(), timestamp(0))
            .await
            .unwrap()
            .unwrap()
            .id;
        fixture
            .storage
            .import_pairs(other, &[pair("x", "один")])
            .await
            .unwrap();
        // Two translations in the first collection and one in the second do
        // not add up to three.
        assert!(
            !fixture
                .storage
                .has_enough_distinct_translations(user_id(1), 3)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn precheck_fails_for_user_without_collections() {
        let fixture = fixture().await;
        assert!(
            !fixture
                .storage
                .has_enough_distinct_translations(user_id(3), 1)
                .await
                .unwrap()
        );
    }
}
