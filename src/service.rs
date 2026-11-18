use std::{future::Future, sync::Arc, time::Duration};

use rand::{Rng, SeedableRng, rngs::StdRng};

use crate::{
    config::Config,
    ids::{MessageId, QuizId, UserId, WordId},
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
        // Options are built only for the picked word; a word that cannot get
        // them is zeroed and the pick repeated, which equals picking among
        // eligible words only.
        loop {
            let index = weights.iter().copied().collect::<Weights>().pick(rng)?;
            let candidate = &candidates[index];
            let pool: Vec<String> = candidates
                .iter()
                .filter(|other| other.collection_id == candidate.collection_id)
                .flat_map(|other| other.translations.iter().cloned())
                .collect();
            match QuizOptions::new(&candidate.translations, &pool, config.quiz.options, rng) {
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
    /// The user paused or rescheduled after being picked as due.
    NotDue,
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
        let candidates = self.storage.candidates(user).await?;
        let mut rng = StdRng::from_os_rng();
        Ok(ChosenQuiz::choose(&candidates, &self.config, now, &mut rng).is_some())
    }

    /// Scheduler path of `dispatch`: pause and schedule are checked again
    /// because they can change between listing due users and sending. `/quiz`
    /// skips this.
    pub async fn dispatch_due(
        &self,
        user: UserId,
        now: Timestamp,
        rng: &mut StdRng,
    ) -> Result<DispatchOutcome, sqlx::Error> {
        match self.storage.get_user(user).await? {
            Some(row) if !row.paused && row.next_quiz_at <= now => {
                self.dispatch(user, now, rng).await
            }
            _ => Ok(DispatchOutcome::NotDue),
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
        let candidates = self.storage.candidates(user).await?;
        let Some(chosen) = ChosenQuiz::choose(&candidates, &self.config, now, rng) else {
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
