use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use rand::{Rng, SeedableRng, rngs::StdRng};

use crate::{
    config::Config,
    ids::{CollectionId, MessageId, QuizId, UserId, WordId},
    quiz::QuizOptions,
    selection::Weights,
    storage::{Candidate, Storage},
    time::Timestamp,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendError {
    /// The user blocked the bot or deleted the chat.
    Blocked,
    Other(String),
}

pub trait QuizSender {
    fn send_quiz(
        &self,
        user: UserId,
        quiz: QuizId,
        term: &str,
        options: &[String],
    ) -> impl Future<Output = Result<MessageId, SendError>> + Send;

    /// Removes the buttons from an earlier quiz message. Best effort: the
    /// message may be gone or too old to edit.
    fn clear_keyboard(&self, user: UserId, message: MessageId) -> impl Future<Output = ()> + Send;
}

pub struct ChosenQuiz {
    pub word_id: WordId,
    pub term: String,
    pub options: QuizOptions,
}

impl ChosenQuiz {
    /// Picks a word by weight among those that can get answer options.
    pub fn choose<R: Rng + ?Sized>(
        candidates: &[Candidate],
        config: &Config,
        now: Timestamp,
        rng: &mut R,
    ) -> Option<ChosenQuiz> {
        let mut weights: Vec<f64> = candidates
            .iter()
            .map(|candidate| config.selection.word_weight(&candidate.answers, now))
            .collect();
        // Distractors come from the word's own collection.
        let mut pools: HashMap<CollectionId, Vec<&String>> = HashMap::new();
        for candidate in candidates {
            pools
                .entry(candidate.collection_id)
                .or_default()
                .extend(&candidate.translations);
        }
        // Options are built only for the picked word; a word that cannot get
        // them is zeroed and the pick repeated, which equals picking among
        // eligible words only.
        loop {
            let index = weights.iter().copied().collect::<Weights>().pick(rng)?;
            let candidate = &candidates[index];
            let pool = &pools[&candidate.collection_id];
            match QuizOptions::new(
                &candidate.translations,
                pool.iter().copied(),
                config.quiz.options,
                rng,
            ) {
                Some(options) => {
                    return Some(ChosenQuiz {
                        word_id: candidate.word_id,
                        term: candidate.term.clone(),
                        options,
                    });
                }
                None => weights[index] = 0.0,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    Sent,
    AlreadyOpen,
    NoEligibleWords,
    Blocked,
    Failed,
}

/// What `/quiz` did for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuizRequest {
    /// A new quiz was sent, or the open one was sent again.
    Sent,
    Blocked,
    NoEligibleWords,
    /// Another quiz became open while this one was being created.
    Raced,
    Failed,
}

impl From<DispatchOutcome> for QuizRequest {
    fn from(outcome: DispatchOutcome) -> QuizRequest {
        match outcome {
            DispatchOutcome::Sent => QuizRequest::Sent,
            DispatchOutcome::AlreadyOpen => QuizRequest::Raced,
            DispatchOutcome::NoEligibleWords => QuizRequest::NoEligibleWords,
            DispatchOutcome::Blocked => QuizRequest::Blocked,
            DispatchOutcome::Failed => QuizRequest::Failed,
        }
    }
}

pub struct QuizService<S> {
    storage: Storage,
    sender: S,
    config: Arc<Config>,
}

impl<S: QuizSender> QuizService<S> {
    pub fn new(storage: Storage, sender: S, config: Arc<Config>) -> Self {
        QuizService {
            storage,
            sender,
            config,
        }
    }

    pub async fn due_users(&self, now: Timestamp) -> Result<Vec<UserId>, sqlx::Error> {
        self.storage.due_users(now).await
    }

    pub fn tick_interval(&self) -> Duration {
        Duration::from_secs(self.config.schedule.tick_seconds)
    }

    /// Whether the user has any word a quiz can be built for.
    pub async fn has_eligible_words(
        &self,
        user: UserId,
        now: Timestamp,
    ) -> Result<bool, sqlx::Error> {
        let mut rng = StdRng::from_os_rng();
        Ok(self.choose_quiz(user, now, &mut rng).await?.is_some())
    }

    /// Scheduler path of `dispatch`: pause and schedule are checked again
    /// because they can change between listing due users and sending. `None`
    /// means the user is not due any more. `/quiz` skips this.
    pub async fn dispatch_due(
        &self,
        user: UserId,
        now: Timestamp,
        rng: &mut StdRng,
    ) -> Result<Option<DispatchOutcome>, sqlx::Error> {
        match self.storage.get_user(user).await? {
            Some(row) if row.is_due(now) => self.dispatch(user, now, rng).await.map(Some),
            _ => Ok(None),
        }
    }

    /// `/quiz`: sends the open quiz again, moving its buttons to the new
    /// message, or else dispatches a new one.
    pub async fn request_quiz(
        &self,
        user: UserId,
        now: Timestamp,
        rng: &mut StdRng,
    ) -> Result<QuizRequest, sqlx::Error> {
        let Some(open) = self.storage.open_quiz(user).await? else {
            return Ok(self.dispatch(user, now, rng).await?.into());
        };
        match self
            .sender
            .send_quiz(user, open.id, &open.term, &open.options)
            .await
        {
            Ok(message_id) => {
                self.storage.set_quiz_message(open.id, message_id).await?;
                if let Some(old_message) = open.message_id {
                    self.sender.clear_keyboard(user, old_message).await;
                }
                Ok(QuizRequest::Sent)
            }
            // The old quiz stays open: only a quiz that was never delivered is
            // deleted.
            Err(error) => Ok(self.handle_send_error(user, error, now).await?.into()),
        }
    }

    pub async fn dispatch(
        &self,
        user: UserId,
        now: Timestamp,
        rng: &mut StdRng,
    ) -> Result<DispatchOutcome, sqlx::Error> {
        if self.storage.open_quiz(user).await?.is_some() {
            return Ok(DispatchOutcome::AlreadyOpen);
        }
        let Some(chosen) = self.choose_quiz(user, now, rng).await? else {
            return Ok(DispatchOutcome::NoEligibleWords);
        };
        let Some(quiz_id) = self
            .storage
            .create_quiz(
                user,
                chosen.word_id,
                chosen.options.options(),
                chosen.options.correct_index(),
                now,
            )
            .await?
        else {
            return Ok(DispatchOutcome::AlreadyOpen);
        };
        let sent = self
            .sender
            .send_quiz(user, quiz_id, &chosen.term, chosen.options.options())
            .await;
        match sent {
            Ok(message_id) => {
                self.storage.set_quiz_message(quiz_id, message_id).await?;
                Ok(DispatchOutcome::Sent)
            }
            Err(error) => {
                self.storage.delete_quiz(quiz_id).await?;
                self.handle_send_error(user, error, now).await
            }
        }
    }

    /// The cheap SQL check goes first: when no collection can supply enough
    /// distinct options, the candidates are not loaded at all.
    async fn choose_quiz(
        &self,
        user: UserId,
        now: Timestamp,
        rng: &mut StdRng,
    ) -> Result<Option<ChosenQuiz>, sqlx::Error> {
        let options = self.config.quiz.options;
        if !self
            .storage
            .has_enough_distinct_translations(user, options)
            .await?
        {
            return Ok(None);
        }
        let candidates = self.storage.candidates(user).await?;
        Ok(ChosenQuiz::choose(&candidates, &self.config, now, rng))
    }

    async fn handle_send_error(
        &self,
        user: UserId,
        error: SendError,
        now: Timestamp,
    ) -> Result<DispatchOutcome, sqlx::Error> {
        match error {
            SendError::Blocked => {
                tracing::info!(%user, "user blocked the bot, pausing quizzes");
                self.storage.set_paused(user, true, now).await?;
                Ok(DispatchOutcome::Blocked)
            }
            SendError::Other(message) => {
                tracing::warn!(%user, %message, "sending quiz failed");
                Ok(DispatchOutcome::Failed)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;

    use super::*;
    use crate::{config::QuizConfig, ids::CollectionId};

    pub fn candidate(word_id: i64, collection_id: i64, term: &str, translation: &str) -> Candidate {
        Candidate {
            word_id: WordId(word_id),
            collection_id: CollectionId(collection_id),
            term: term.to_owned(),
            translations: vec![translation.to_owned()],
            answers: Vec::new(),
        }
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_secs(secs)
    }

    #[test]
    fn distractors_come_from_same_collection() {
        let candidates = [
            candidate(1, 10, "a", "aa"),
            candidate(2, 10, "b", "bb"),
            candidate(3, 20, "c", "cc"),
            candidate(4, 20, "d", "dd"),
        ];
        let mut rng = StdRng::seed_from_u64(1);
        let config = Config {
            quiz: QuizConfig { options: 2 },
            ..Config::for_tests()
        };
        for _ in 0..50 {
            let quiz = ChosenQuiz::choose(&candidates, &config, ts(0), &mut rng).unwrap();
            let allowed: &[&str] = if quiz.word_id.0 <= 2 {
                &["aa", "bb"]
            } else {
                &["cc", "dd"]
            };
            assert!(
                quiz.options
                    .options()
                    .iter()
                    .all(|option| allowed.contains(&option.as_str()))
            );
        }
    }

    #[test]
    fn word_without_enough_distractors_is_skipped() {
        let candidates = [candidate(1, 10, "a", "aa"), candidate(2, 20, "b", "bb")];
        let mut rng = StdRng::seed_from_u64(1);
        assert!(ChosenQuiz::choose(&candidates, &Config::for_tests(), ts(0), &mut rng).is_none());
    }

    #[test]
    fn only_eligible_words_are_picked() {
        // Word 9 sits alone in its collection, so it must never be chosen.
        let candidates = [
            candidate(1, 10, "a", "aa"),
            candidate(2, 10, "b", "bb"),
            candidate(3, 10, "c", "cc"),
            candidate(9, 20, "z", "zz"),
        ];
        let mut rng = StdRng::seed_from_u64(3);
        for _ in 0..50 {
            let quiz =
                ChosenQuiz::choose(&candidates, &Config::for_tests(), ts(0), &mut rng).unwrap();
            assert_ne!(quiz.word_id, WordId(9));
        }
    }
}
