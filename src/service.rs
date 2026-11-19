use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::Arc,
    time::Duration,
};

use rand::{Rng, SeedableRng, rngs::StdRng};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task,
};

use crate::{
    config::Config,
    ids::{CollectionId, MessageId, QuizId, UserId, WordId},
    import::NormalizedText,
    quiz::QuizOptions,
    selection::Weights,
    storage::{Candidate, Storage},
    time::Timestamp,
};

/// Each selection can hold a blocking thread and a user's whole vocabulary in
/// memory, so only a few run at once.
const MAX_CONCURRENT_SELECTIONS: usize = 4;

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
        random_generator: &mut R,
    ) -> Option<ChosenQuiz> {
        let distractors_needed = config.quiz.options.saturating_sub(1);
        let eligible = ChosenQuiz::eligible_words(candidates, distractors_needed);
        let weights: Weights = candidates
            .iter()
            .zip(&eligible)
            .map(|(candidate, &is_eligible)| {
                if is_eligible {
                    config.selection.word_weight(&candidate.answers, now)
                } else {
                    0.0
                }
            })
            .collect();
        let candidate = &candidates[weights.pick(random_generator)?];
        // Distractors come from the word's own collection.
        let pool = candidates
            .iter()
            .filter(|other| other.collection_id == candidate.collection_id)
            .flat_map(|other| &other.translations);
        let options = QuizOptions::new(
            &candidate.translations,
            pool,
            config.quiz.options,
            random_generator,
        )?;
        Some(ChosenQuiz {
            word_id: candidate.word_id,
            term: candidate.term.clone(),
            options,
        })
    }

    /// Whether each candidate can get `distractors_needed` distractors from its
    /// collection.
    fn eligible_words(candidates: &[Candidate], distractors_needed: usize) -> Vec<bool> {
        let mut collections: HashMap<CollectionId, Vec<usize>> = HashMap::new();
        for (position, candidate) in candidates.iter().enumerate() {
            collections
                .entry(candidate.collection_id)
                .or_default()
                .push(position);
        }
        let mut eligible = vec![false; candidates.len()];
        for positions in collections.values() {
            let longest = positions
                .iter()
                .map(|&position| candidates[position].translations.len())
                .max()
                .unwrap_or(0);
            // With this many distinct translations every word has enough
            // others, so normalizing the rest of the collection would change
            // nothing.
            let enough = longest + distractors_needed;
            let mut distinct: HashSet<NormalizedText> = HashSet::new();
            let mut own_counts = Vec::with_capacity(positions.len());
            for &position in positions {
                if distinct.len() >= enough {
                    break;
                }
                let own: HashSet<NormalizedText> = candidates[position]
                    .translations
                    .iter()
                    .map(|translation| NormalizedText::new(translation))
                    .collect();
                own_counts.push(own.len());
                distinct.extend(own);
            }
            let is_saturated = distinct.len() >= enough;
            for (index, &position) in positions.iter().enumerate() {
                let has_translations = !candidates[position].translations.is_empty();
                eligible[position] = has_translations
                    && (is_saturated || distinct.len() - own_counts[index] >= distractors_needed);
            }
        }
        eligible
    }
}

/// What choosing a word for the user came to.
enum Selection {
    Chosen(ChosenQuiz),
    NoEligibleWords,
    /// The selection task died; already logged.
    Failed,
}

/// What an attempt to send a quiz to the user did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// A new quiz was sent, or the open one was sent again.
    Sent,
    /// Another quiz became open first.
    AlreadyOpen,
    NoEligibleWords,
    Blocked,
    Failed,
}

#[derive(Clone)]
pub struct QuizService<S> {
    storage: Storage,
    sender: S,
    config: Arc<Config>,
    selections: Arc<Semaphore>,
    /// Runs inside the selection task, to observe or disturb it.
    #[cfg(test)]
    selection_probe: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl<S: QuizSender> QuizService<S> {
    pub fn new(storage: Storage, sender: S, config: Arc<Config>) -> Self {
        QuizService {
            storage,
            sender,
            config,
            selections: Arc::new(Semaphore::new(MAX_CONCURRENT_SELECTIONS)),
            #[cfg(test)]
            selection_probe: None,
        }
    }

    pub async fn due_users(&self, now: Timestamp) -> Result<Vec<UserId>, sqlx::Error> {
        self.storage.due_users(now).await
    }

    pub fn tick_interval(&self) -> Duration {
        self.config.schedule.tick_seconds.as_duration()
    }

    /// Whether the user has any word a quiz can be built for. A failed check
    /// answers `true`, so the user is not told to add words they already have.
    pub async fn has_eligible_words(
        &self,
        user: UserId,
        now: Timestamp,
    ) -> Result<bool, sqlx::Error> {
        let mut random_generator: StdRng = rand::make_rng();
        let selection = self.choose_quiz(user, now, &mut random_generator).await?;
        Ok(!matches!(selection, Selection::NoEligibleWords))
    }

    /// Scheduler path of `dispatch`: pause and schedule are checked again
    /// because they can change between listing due users and sending. `None`
    /// means the user is not due any more. `/quiz` skips this.
    pub async fn dispatch_due(
        &self,
        user: UserId,
        now: Timestamp,
        random_generator: &mut StdRng,
    ) -> Result<Option<DispatchOutcome>, sqlx::Error> {
        match self.storage.get_user(user).await? {
            Some(row) if row.is_due(now) => {
                self.dispatch(user, now, random_generator).await.map(Some)
            }
            _ => Ok(None),
        }
    }

    /// `/quiz`: sends the open quiz again, moving its buttons to the new
    /// message, or else dispatches a new one.
    pub async fn request_quiz(
        &self,
        user: UserId,
        now: Timestamp,
        random_generator: &mut StdRng,
    ) -> Result<DispatchOutcome, sqlx::Error> {
        let Some(open) = self.storage.open_quiz(user).await? else {
            return self.dispatch(user, now, random_generator).await;
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
                Ok(DispatchOutcome::Sent)
            }
            // The old quiz stays open: only a quiz that was never delivered is
            // deleted.
            Err(error) => self.handle_send_error(user, error, now).await,
        }
    }

    pub async fn dispatch(
        &self,
        user: UserId,
        now: Timestamp,
        random_generator: &mut StdRng,
    ) -> Result<DispatchOutcome, sqlx::Error> {
        if self.storage.open_quiz(user).await?.is_some() {
            return Ok(DispatchOutcome::AlreadyOpen);
        }
        let chosen = match self.choose_quiz(user, now, random_generator).await? {
            Selection::Chosen(chosen) => chosen,
            Selection::NoEligibleWords => return Ok(DispatchOutcome::NoEligibleWords),
            Selection::Failed => return Ok(DispatchOutcome::Failed),
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
        random_generator: &mut StdRng,
    ) -> Result<Selection, sqlx::Error> {
        let options = self.config.quiz.options;
        if !self
            .storage
            .has_enough_distinct_translations(user, options)
            .await?
        {
            return Ok(Selection::NoEligibleWords);
        }
        // Taken before loading: the candidates are what fills the memory.
        let Ok(permit) = Arc::clone(&self.selections).acquire_owned().await else {
            tracing::error!(%user, "quiz selection is shut down");
            return Ok(Selection::Failed);
        };
        let candidates = self.storage.candidates(user).await?;
        Ok(self
            .select_off_async_workers(user, candidates, now, random_generator, permit)
            .await)
    }

    /// Handlers of all users share one task, so CPU work here would stall them
    /// all. A panic stays inside the blocking task and fails this user only.
    async fn select_off_async_workers(
        &self,
        user: UserId,
        candidates: Vec<Candidate>,
        now: Timestamp,
        random_generator: &mut StdRng,
        permit: OwnedSemaphorePermit,
    ) -> Selection {
        let config = Arc::clone(&self.config);
        let mut task_generator = StdRng::from_rng(random_generator);
        #[cfg(test)]
        let probe = self.selection_probe.clone();
        let selection = task::spawn_blocking(move || {
            // Held to the end even if the caller stops waiting for the result.
            let _permit = permit;
            #[cfg(test)]
            if let Some(probe) = probe {
                probe();
            }
            ChosenQuiz::choose(&candidates, &config, now, &mut task_generator)
        })
        .await;
        match selection {
            Ok(Some(chosen)) => Selection::Chosen(chosen),
            Ok(None) => Selection::NoEligibleWords,
            Err(error) => {
                tracing::error!(%user, %error, "quiz selection failed");
                Selection::Failed
            }
        }
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
    use std::{
        convert::Infallible,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
    };

    use rand::{RngExt, TryRng};
    use tokio::sync::oneshot;

    use super::*;
    use crate::{
        config::QuizConfig,
        import::ParsedPair,
        period::Period,
        testing::{FAKE_MESSAGE_ID, FakeSender, QuizFixture, timestamp},
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

    impl TryRng for CountingGenerator {
        type Error = Infallible;

        fn try_next_u32(&mut self) -> Result<u32, Infallible> {
            self.draws += 1;
            self.inner.try_next_u32()
        }

        fn try_next_u64(&mut self) -> Result<u64, Infallible> {
            self.draws += 1;
            self.inner.try_next_u64()
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Infallible> {
            self.draws += 1;
            self.inner.try_fill_bytes(destination)
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
        let chosen = ChosenQuiz::choose(
            &candidates,
            &Config::for_tests(),
            timestamp(0),
            &mut generator,
        );
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
            let chosen = ChosenQuiz::choose(
                candidates,
                &Config::for_tests(),
                timestamp(0),
                &mut generator,
            )
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
        let mut random_generator = StdRng::seed_from_u64(1);
        let config = Config {
            quiz: QuizConfig { options: 2 },
            ..Config::for_tests()
        };
        for _ in 0..50 {
            let quiz =
                ChosenQuiz::choose(&candidates, &config, timestamp(0), &mut random_generator)
                    .unwrap();
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
        let mut random_generator = StdRng::seed_from_u64(1);
        assert!(
            ChosenQuiz::choose(
                &candidates,
                &Config::for_tests(),
                timestamp(0),
                &mut random_generator
            )
            .is_none()
        );
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
        let mut random_generator = StdRng::seed_from_u64(3);
        for _ in 0..50 {
            let quiz = ChosenQuiz::choose(
                &candidates,
                &Config::for_tests(),
                timestamp(0),
                &mut random_generator,
            )
            .unwrap();
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
                    .set_paused(UserId(1), true, timestamp(50))
                    .await
                    .unwrap();
            } else {
                fixture
                    .storage
                    .set_period(UserId(1), Period::from_secs(600).unwrap(), timestamp(50))
                    .await
                    .unwrap();
            }
            let outcome = fixture
                .service()
                .dispatch_due(UserId(1), timestamp(100), &mut fixture.random_generator())
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
            .dispatch(UserId(1), timestamp(100), &mut fixture.random_generator())
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
        assert!(
            fixture
                .storage
                .due_users(timestamp(159))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture.storage.due_users(timestamp(160)).await.unwrap(),
            [UserId(1)]
        );
    }

    #[tokio::test]
    async fn quiz_cancelled_during_send_loses_its_buttons() {
        let fixture = QuizFixture::with_words(1, 4).await;
        fixture.sender.cancel_during_send(fixture.storage.clone());
        fixture
            .service()
            .dispatch(UserId(1), timestamp(100), &mut fixture.random_generator())
            .await
            .unwrap();
        assert_eq!(fixture.sender.cleared(), [(UserId(1), FAKE_MESSAGE_ID)]);
    }

    #[tokio::test]
    async fn manual_dispatch_ignores_pause() {
        let fixture = QuizFixture::with_words(1, 4).await;
        fixture
            .storage
            .set_paused(UserId(1), true, timestamp(50))
            .await
            .unwrap();
        let outcome = fixture
            .service()
            .dispatch(UserId(1), timestamp(100), &mut fixture.random_generator())
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
                .has_eligible_words(UserId(1), timestamp(100))
                .await
                .unwrap()
        );
        assert_eq!(
            service
                .dispatch(UserId(1), timestamp(100), &mut fixture.random_generator())
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
                .has_eligible_words(UserId(1), timestamp(100))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn request_resends_open_quiz_and_clears_old_buttons() {
        let fixture = QuizFixture::with_words(1, 4).await;
        let service = fixture.service();
        let first = service
            .request_quiz(UserId(1), timestamp(100), &mut fixture.random_generator())
            .await
            .unwrap();
        assert_eq!(first, DispatchOutcome::Sent);
        assert!(fixture.sender.cleared().is_empty());
        let second = service
            .request_quiz(UserId(1), timestamp(101), &mut fixture.random_generator())
            .await
            .unwrap();
        assert_eq!(second, DispatchOutcome::Sent);
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
            .request_quiz(UserId(1), timestamp(100), &mut fixture.random_generator())
            .await
            .unwrap();
        fixture.sender.fail(UserId(1), SendError::Blocked);
        let outcome = service
            .request_quiz(UserId(1), timestamp(101), &mut fixture.random_generator())
            .await
            .unwrap();
        assert_eq!(outcome, DispatchOutcome::Blocked);
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

    #[test]
    fn eligibility_matches_exhaustive_check() {
        let mut random_generator = StdRng::seed_from_u64(5);
        let vocabulary = ["aa", "AA", " aa ", "bb", "cc", "dd", "ee", "ff"];
        for _ in 0..500 {
            let candidates: Vec<Candidate> = (0..random_generator.random_range(1..12))
                .map(|word_id| Candidate {
                    translations: (0..random_generator.random_range(0..4))
                        .map(|_| {
                            vocabulary[random_generator.random_range(0..vocabulary.len())]
                                .to_owned()
                        })
                        .collect(),
                    ..candidate(word_id, random_generator.random_range(1..3), "term", "aa")
                })
                .collect();
            let needed = random_generator.random_range(1..4);
            let expected: Vec<bool> = candidates
                .iter()
                .map(|word| {
                    let normalized = |other: &Candidate| -> HashSet<NormalizedText> {
                        other
                            .translations
                            .iter()
                            .map(|translation| NormalizedText::new(translation))
                            .collect()
                    };
                    let own = normalized(word);
                    let others: HashSet<NormalizedText> = candidates
                        .iter()
                        .filter(|other| other.collection_id == word.collection_id)
                        .flat_map(normalized)
                        .filter(|translation| !own.contains(translation))
                        .collect();
                    !own.is_empty() && others.len() >= needed
                })
                .collect();
            assert_eq!(
                ChosenQuiz::eligible_words(&candidates, needed),
                expected,
                "{candidates:?} needing {needed}"
            );
        }
    }

    /// Blocks the selection task until the test releases it, recording whether
    /// the release arrived while the task was waiting.
    struct HeldSelection {
        entered: oneshot::Receiver<()>,
        release: mpsc::Sender<()>,
        was_released: Arc<AtomicBool>,
    }

    impl HeldSelection {
        fn install(service: &mut QuizService<FakeSender>) -> HeldSelection {
            let (entered_sender, entered) = oneshot::channel();
            let (release, released) = mpsc::channel();
            let was_released = Arc::new(AtomicBool::new(false));
            let entered_sender = Mutex::new(Some(entered_sender));
            let released = Mutex::new(released);
            let record = Arc::clone(&was_released);
            service.selection_probe = Some(Arc::new(move || {
                if let Some(sender) = entered_sender.lock().unwrap().take() {
                    let _ = sender.send(());
                }
                let waited = released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5));
                record.store(waited.is_ok(), Ordering::SeqCst);
            }));
            HeldSelection {
                entered,
                release,
                was_released,
            }
        }

        /// On this single-threaded runtime the release can only be sent if the
        /// selection left the thread free for other tasks.
        async fn assert_other_work_runs<T>(self, selection: impl Future<Output = T>) -> T {
            let HeldSelection {
                entered,
                release,
                was_released,
            } = self;
            let other_work = async move {
                entered.await.unwrap();
                release.send(()).unwrap();
            };
            let (result, ()) = tokio::join!(selection, other_work);
            assert!(was_released.load(Ordering::SeqCst));
            result
        }
    }

    #[tokio::test]
    async fn other_work_runs_during_selection_for_every_caller() {
        let fixture = QuizFixture::with_words(1, 4).await;
        let mut service = fixture.service();
        let held = HeldSelection::install(&mut service);
        let eligible = held
            .assert_other_work_runs(service.has_eligible_words(UserId(1), timestamp(100)))
            .await;
        assert!(eligible.unwrap());

        let held = HeldSelection::install(&mut service);
        let mut random_generator = fixture.random_generator();
        let outcome = held
            .assert_other_work_runs(service.dispatch(
                UserId(1),
                timestamp(100),
                &mut random_generator,
            ))
            .await;
        assert_eq!(outcome.unwrap(), DispatchOutcome::Sent);

        let fixture = QuizFixture::with_words(1, 4).await;
        let mut service = fixture.service();
        let held = HeldSelection::install(&mut service);
        let mut random_generator = fixture.random_generator();
        let outcome = held
            .assert_other_work_runs(service.request_quiz(
                UserId(1),
                timestamp(100),
                &mut random_generator,
            ))
            .await;
        assert_eq!(outcome.unwrap(), DispatchOutcome::Sent);
    }

    #[tokio::test]
    async fn selection_panic_fails_only_that_attempt() {
        let fixture = QuizFixture::with_words(2, 4).await;
        let mut service = fixture.service();
        service.selection_probe = Some(Arc::new(|| panic!("selection failure under test")));
        for _ in 0..=MAX_CONCURRENT_SELECTIONS {
            let outcome = service
                .dispatch(UserId(1), timestamp(100), &mut fixture.random_generator())
                .await
                .unwrap();
            assert_eq!(outcome, DispatchOutcome::Failed);
        }
        assert!(
            service
                .has_eligible_words(UserId(1), timestamp(100))
                .await
                .unwrap()
        );
        assert!(
            fixture
                .storage
                .open_quiz(UserId(1))
                .await
                .unwrap()
                .is_none()
        );
        // Every permit came back, so later selections still run.
        service.selection_probe = None;
        let outcome = service
            .dispatch(UserId(2), timestamp(100), &mut fixture.random_generator())
            .await
            .unwrap();
        assert_eq!(outcome, DispatchOutcome::Sent);
    }

    #[tokio::test]
    async fn concurrent_selections_stay_within_limit() {
        let fixture = QuizFixture::with_words(1, 4).await;
        let mut service = fixture.service();
        let running = Arc::new(AtomicUsize::new(0));
        let highest = Arc::new(AtomicUsize::new(0));
        let (running_count, highest_count) = (Arc::clone(&running), Arc::clone(&highest));
        service.selection_probe = Some(Arc::new(move || {
            let now_running = running_count.fetch_add(1, Ordering::SeqCst) + 1;
            highest_count.fetch_max(now_running, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            running_count.fetch_sub(1, Ordering::SeqCst);
        }));
        let tasks: Vec<_> = (0..3 * MAX_CONCURRENT_SELECTIONS)
            .map(|_| {
                let service = service.clone();
                tokio::spawn(async move {
                    service
                        .has_eligible_words(UserId(1), timestamp(100))
                        .await
                        .unwrap()
                })
            })
            .collect();
        for task in tasks {
            assert!(task.await.unwrap());
        }
        assert!(highest.load(Ordering::SeqCst) <= MAX_CONCURRENT_SELECTIONS);
    }
}
