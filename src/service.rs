use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::Arc,
    time::Duration,
};

use rand::{Rng, SeedableRng, rngs::StdRng};

use crate::{
    config::Config,
    ids::{CollectionId, MessageId, QuizId, UserId, WordId},
    import::Normalized,
    quiz::QuizOptions,
    selection::Weights,
    storage::{Candidate, Storage},
    time::Timestamp,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendError {
    /// The user blocked the bot or deleted the chat.
    Blocked,
    /// Telegram refused this message for good, so repeating it cannot help.
    Rejected(String),
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

/// Distinct translations of one collection, the source of distractors.
#[derive(Default)]
struct TranslationPool<'a> {
    normalized: HashSet<Normalized>,
    translations: Vec<&'a String>,
}

impl<'a> TranslationPool<'a> {
    fn add(&mut self, translation: &'a String) {
        if self.normalized.insert(Normalized::new(translation)) {
            self.translations.push(translation);
        }
    }

    /// Whether the pool holds `needed` distractors for a word with these
    /// translations. All of them are in the pool, so the rest are distractors.
    fn has_distractors(&self, own: &[String], needed: usize) -> bool {
        let own: HashSet<Normalized> = own
            .iter()
            .map(|translation| Normalized::new(translation))
            .collect();
        !own.is_empty() && self.normalized.len() - own.len() >= needed
    }
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
        // Distractors come from the word's own collection.
        let mut pools: HashMap<CollectionId, TranslationPool> = HashMap::new();
        for candidate in candidates {
            let pool = pools.entry(candidate.collection_id).or_default();
            candidate
                .translations
                .iter()
                .for_each(|translation| pool.add(translation));
        }
        let distractors_needed = config.quiz.options.saturating_sub(1);
        // Eligibility is decided up front, so the cost stays linear in the
        // number of words however many of them cannot get options.
        let weights: Weights = candidates
            .iter()
            .map(|candidate| {
                let pool = &pools[&candidate.collection_id];
                if pool.has_distractors(&candidate.translations, distractors_needed) {
                    config.selection.word_weight(&candidate.answers, now)
                } else {
                    0.0
                }
            })
            .collect();
        let candidate = &candidates[weights.pick(rng)?];
        let options = QuizOptions::new(
            &candidate.translations,
            pools[&candidate.collection_id].translations.iter().copied(),
            config.quiz.options,
            rng,
        )?;
        Some(ChosenQuiz {
            word_id: candidate.word_id,
            term: candidate.term.clone(),
            options,
        })
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
                self.store_message(user, open.id, message_id).await?;
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
                self.store_message(user, quiz_id, message_id).await?;
                Ok(DispatchOutcome::Sent)
            }
            Err(error) => {
                self.storage.delete_quiz(quiz_id).await?;
                self.handle_send_error(user, error, now).await
            }
        }
    }

    /// A quiz cancelled while its send was in flight has no row to hold the
    /// message, so nothing else would ever remove these buttons.
    async fn store_message(
        &self,
        user: UserId,
        quiz: QuizId,
        message: MessageId,
    ) -> Result<(), sqlx::Error> {
        if !self.storage.set_quiz_message(quiz, message).await? {
            self.sender.clear_keyboard(user, message).await;
        }
        Ok(())
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
            // The user stays due otherwise, and every tick would fail the same
            // way.
            SendError::Rejected(message) => {
                tracing::warn!(%user, %message, "telegram rejected quiz, postponing user");
                self.storage.postpone_user(user, now).await?;
                Ok(DispatchOutcome::Failed)
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
    use rand::{RngCore, SeedableRng};

    use super::*;
    use crate::{
        config::QuizConfig,
        import::ParsedPair,
        period::Period,
        testing::{FAKE_MESSAGE_ID, QuizFixture, ts},
    };

    pub fn candidate(word_id: i64, collection_id: i64, term: &str, translation: &str) -> Candidate {
        Candidate {
            word_id: WordId(word_id),
            collection_id: CollectionId(collection_id),
            term: term.to_owned(),
            translations: vec![translation.to_owned()],
            answers: Vec::new(),
        }
    }

    /// Counts generator draws: a retry per ineligible word shows up as draws
    /// growing with the word count, without relying on wall-clock time.
    struct CountingGenerator {
        inner: StdRng,
        draws: usize,
    }

    impl CountingGenerator {
        fn new() -> CountingGenerator {
            CountingGenerator {
                inner: StdRng::seed_from_u64(1),
                draws: 0,
            }
        }
    }

    impl RngCore for CountingGenerator {
        fn next_u32(&mut self) -> u32 {
            self.draws += 1;
            self.inner.next_u32()
        }

        fn next_u64(&mut self) -> u64 {
            self.draws += 1;
            self.inner.next_u64()
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            self.draws += 1;
            self.inner.fill_bytes(destination);
        }
    }

    /// Holds every translation of the collection, so it has no distractors.
    fn candidate_without_distractors(word_id: i64) -> Candidate {
        Candidate {
            translations: ["aa", "bb", "cc"].map(String::from).to_vec(),
            ..candidate(word_id, 10, "term", "aa")
        }
    }

    #[test]
    fn ineligible_words_cost_no_retries() {
        let candidates: Vec<Candidate> = (0..2000).map(candidate_without_distractors).collect();
        let mut generator = CountingGenerator::new();
        let chosen = ChosenQuiz::choose(&candidates, &Config::for_tests(), ts(0), &mut generator);
        assert!(chosen.is_none());
        assert_eq!(generator.draws, 0);
    }

    #[test]
    fn eligible_word_is_found_among_many_ineligible_without_retries() {
        let build = |ineligible: i64| -> Vec<Candidate> {
            let mut candidates: Vec<Candidate> =
                (0..ineligible).map(candidate_without_distractors).collect();
            candidates.push(candidate(ineligible, 10, "fits", "dd"));
            candidates
        };
        let draws_for = |candidates: &[Candidate]| {
            let mut generator = CountingGenerator::new();
            let chosen =
                ChosenQuiz::choose(candidates, &Config::for_tests(), ts(0), &mut generator)
                    .unwrap();
            assert_eq!(chosen.term, "fits");
            generator.draws
        };
        assert_eq!(draws_for(&build(10)), draws_for(&build(2000)));
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

    #[tokio::test]
    async fn dispatch_due_skips_user_no_longer_due() {
        for paused in [true, false] {
            let fixture = QuizFixture::with_words(1, 4).await;
            if paused {
                fixture
                    .storage
                    .set_paused(UserId(1), true, ts(50))
                    .await
                    .unwrap();
            } else {
                fixture
                    .storage
                    .set_period(UserId(1), Period::from_secs(600).unwrap(), ts(50))
                    .await
                    .unwrap();
            }
            let outcome = fixture
                .service()
                .dispatch_due(UserId(1), ts(100), &mut fixture.rng())
                .await
                .unwrap();
            assert_eq!(outcome, None, "paused: {paused}");
            assert!(fixture.sender.sent().is_empty(), "paused: {paused}");
        }
    }

    #[tokio::test]
    async fn rejected_send_postpones_user_by_one_period() {
        let fixture = QuizFixture::with_words(1, 4).await;
        fixture
            .sender
            .fail(UserId(1), SendError::Rejected("message is too long".into()));
        fixture
            .service()
            .dispatch(UserId(1), ts(100), &mut fixture.rng())
            .await
            .unwrap();
        assert!(
            fixture
                .storage
                .open_quiz(UserId(1))
                .await
                .unwrap()
                .is_none()
        );
        assert!(fixture.storage.due_users(ts(159)).await.unwrap().is_empty());
        assert_eq!(
            fixture.storage.due_users(ts(160)).await.unwrap(),
            [UserId(1)]
        );
    }

    #[tokio::test]
    async fn quiz_cancelled_during_send_loses_its_buttons() {
        let fixture = QuizFixture::with_words(1, 4).await;
        fixture.sender.cancel_during_send(fixture.storage.clone());
        fixture
            .service()
            .dispatch(UserId(1), ts(100), &mut fixture.rng())
            .await
            .unwrap();
        assert_eq!(fixture.sender.cleared(), [(UserId(1), FAKE_MESSAGE_ID)]);
    }

    #[tokio::test]
    async fn manual_dispatch_ignores_pause() {
        let fixture = QuizFixture::with_words(1, 4).await;
        fixture
            .storage
            .set_paused(UserId(1), true, ts(50))
            .await
            .unwrap();
        let outcome = fixture
            .service()
            .dispatch(UserId(1), ts(100), &mut fixture.rng())
            .await
            .unwrap();
        assert_eq!(outcome, DispatchOutcome::Sent);
    }

    #[tokio::test]
    async fn repeated_translations_give_no_eligible_words() {
        let fixture = QuizFixture::with_words(1, 0).await;
        let pairs: Vec<ParsedPair> = (0..6)
            .map(|number| ParsedPair {
                term: format!("term{number}"),
                translation: "одно".into(),
            })
            .collect();
        fixture
            .storage
            .import_pairs(fixture.collection, &pairs)
            .await
            .unwrap();
        let service = fixture.service();
        assert!(
            !service
                .has_eligible_words(UserId(1), ts(100))
                .await
                .unwrap()
        );
        assert_eq!(
            service
                .dispatch(UserId(1), ts(100), &mut fixture.rng())
                .await
                .unwrap(),
            DispatchOutcome::NoEligibleWords
        );
        assert!(fixture.sender.sent().is_empty());
    }

    #[tokio::test]
    async fn passed_precheck_still_needs_fitting_word() {
        // Three distinct translations, but all on one word: they are all
        // excluded as synonyms, so no distractors remain.
        let fixture = QuizFixture::with_words(1, 0).await;
        let pairs: Vec<ParsedPair> = ["один", "два", "три"]
            .into_iter()
            .map(|translation| ParsedPair {
                term: "word".into(),
                translation: translation.into(),
            })
            .collect();
        fixture
            .storage
            .import_pairs(fixture.collection, &pairs)
            .await
            .unwrap();
        assert!(
            fixture
                .storage
                .has_enough_distinct_translations(UserId(1), 3)
                .await
                .unwrap()
        );
        assert!(
            !fixture
                .service()
                .has_eligible_words(UserId(1), ts(100))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn request_resends_open_quiz_and_clears_old_buttons() {
        let fixture = QuizFixture::with_words(1, 4).await;
        let service = fixture.service();
        let first = service
            .request_quiz(UserId(1), ts(100), &mut fixture.rng())
            .await
            .unwrap();
        assert_eq!(first, QuizRequest::Sent);
        assert!(fixture.sender.cleared().is_empty());
        let second = service
            .request_quiz(UserId(1), ts(101), &mut fixture.rng())
            .await
            .unwrap();
        assert_eq!(second, QuizRequest::Sent);
        let sent = fixture.sender.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], sent[1]);
        assert_eq!(fixture.sender.cleared(), [(UserId(1), FAKE_MESSAGE_ID)]);
    }

    #[tokio::test]
    async fn blocked_resend_pauses_user_and_keeps_quiz() {
        let fixture = QuizFixture::with_words(1, 4).await;
        let service = fixture.service();
        service
            .request_quiz(UserId(1), ts(100), &mut fixture.rng())
            .await
            .unwrap();
        fixture.sender.fail(UserId(1), SendError::Blocked);
        let outcome = service
            .request_quiz(UserId(1), ts(101), &mut fixture.rng())
            .await
            .unwrap();
        assert_eq!(outcome, QuizRequest::Blocked);
        assert!(
            fixture
                .storage
                .get_user(UserId(1))
                .await
                .unwrap()
                .unwrap()
                .paused
        );
        assert!(
            fixture
                .storage
                .open_quiz(UserId(1))
                .await
                .unwrap()
                .is_some()
        );
    }
}
