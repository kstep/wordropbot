use std::future::Future;
use std::sync::Arc;

use rand::Rng;
use rand::rngs::StdRng;

use crate::config::Config;
use crate::ids::{MessageId, QuizId, UserId, WordId};
use crate::quiz::QuizOptions;
use crate::storage::{Candidate, Storage};
use crate::time::Timestamp;

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
        todo!()
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

    pub async fn dispatch(
        &self,
        user: UserId,
        now: Timestamp,
        rng: &mut StdRng,
    ) -> Result<DispatchOutcome, sqlx::Error> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QuizConfig;
    use crate::ids::CollectionId;
    use rand::SeedableRng;

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
    fn distractors_come_from_the_same_collection() {
        let cands = [
            candidate(1, 10, "a", "aa"),
            candidate(2, 10, "b", "bb"),
            candidate(3, 20, "c", "cc"),
            candidate(4, 20, "d", "dd"),
        ];
        let mut rng = StdRng::seed_from_u64(1);
        let cfg = Config {
            quiz: QuizConfig { options: 2 },
            ..Config::for_tests()
        };
        for _ in 0..50 {
            let q = ChosenQuiz::choose(&cands, &cfg, ts(0), &mut rng).unwrap();
            let allowed: &[&str] = if q.word_id.0 <= 2 {
                &["aa", "bb"]
            } else {
                &["cc", "dd"]
            };
            assert!(
                q.options
                    .options()
                    .iter()
                    .all(|o| allowed.contains(&o.as_str()))
            );
        }
    }

    #[test]
    fn word_without_enough_distractors_is_skipped() {
        let cands = [candidate(1, 10, "a", "aa"), candidate(2, 20, "b", "bb")];
        let mut rng = StdRng::seed_from_u64(1);
        assert!(ChosenQuiz::choose(&cands, &Config::for_tests(), ts(0), &mut rng).is_none());
    }

    #[test]
    fn only_eligible_words_are_picked() {
        // Word 9 sits alone in its collection, so it must never be chosen.
        let cands = [
            candidate(1, 10, "a", "aa"),
            candidate(2, 10, "b", "bb"),
            candidate(3, 10, "c", "cc"),
            candidate(9, 20, "z", "zz"),
        ];
        let mut rng = StdRng::seed_from_u64(3);
        for _ in 0..50 {
            let q = ChosenQuiz::choose(&cands, &Config::for_tests(), ts(0), &mut rng).unwrap();
            assert_ne!(q.word_id, WordId(9));
        }
    }
}
