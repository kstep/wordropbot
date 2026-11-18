use rand::{SeedableRng, rngs::StdRng};
use tokio::{
    sync::watch,
    time::{self, MissedTickBehavior},
};

use crate::{
    service::{DispatchOutcome, QuizSender, QuizService},
    time::Timestamp,
};

pub struct Scheduler<S> {
    service: QuizService<S>,
}

impl<S: QuizSender> Scheduler<S> {
    pub fn new(service: QuizService<S>) -> Self {
        Scheduler { service }
    }

    /// Returns the number of quizzes sent; stops walking users once `shutdown`
    /// is true.
    pub async fn tick(
        &self,
        now: Timestamp,
        rng: &mut StdRng,
        shutdown: &watch::Receiver<bool>,
    ) -> Result<usize, sqlx::Error> {
        let mut sent = 0;
        for user in self.service.due_users(now).await? {
            // A long walk over users must not delay shutdown.
            if *shutdown.borrow() {
                break;
            }
            match self.service.dispatch_due(user, now, rng).await {
                Ok(Some(DispatchOutcome::Sent)) => sent += 1,
                Ok(_) => {}
                Err(error) => tracing::warn!(%user, %error, "quiz dispatch failed"),
            }
        }
        Ok(sent)
    }

    /// Ticks until `shutdown` turns true or its sender is dropped.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let mut rng = StdRng::from_os_rng();
        let mut interval = time::interval(self.service.tick_interval());
        // A slow tick should not be followed by a burst of catch-up ticks.
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // `changed()` only reports later changes, so a shutdown sent before
        // start needs this check.
        if *shutdown.borrow() {
            return;
        }
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(error) = self.tick(Timestamp::now(), &mut rng, &shutdown).await {
                        tracing::error!(%error, "scheduler tick failed");
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        future::{self, Future},
        sync::{Arc, Mutex},
        time::Duration,
    };

    use super::*;
    use crate::{
        code::Code,
        config::Config,
        ids::{CollectionId, MessageId, QuizId, UserId},
        import::ParsedPair,
        period::Period,
        service::{QuizRequest, SendError},
        storage::Storage,
    };

    const FAKE_MESSAGE_ID: MessageId = MessageId(777);

    #[derive(Default)]
    struct FakeState {
        sent: Vec<(UserId, String)>,
        cleared: Vec<(UserId, MessageId)>,
        /// Signalled after each successful send, to stop in mid-walk.
        stop_after_send: Option<watch::Sender<bool>>,
        failures: HashMap<UserId, SendError>,
        /// Skips the user's quiz in the middle of the next send.
        cancel_during_send: Option<Storage>,
    }

    #[derive(Clone, Default)]
    struct FakeSender(Arc<Mutex<FakeState>>);

    impl FakeSender {
        fn fail(&self, user: i64, error: SendError) {
            self.0.lock().unwrap().failures.insert(UserId(user), error);
        }

        fn sent(&self) -> Vec<(UserId, String)> {
            self.0.lock().unwrap().sent.clone()
        }

        fn stop_after_send(&self, stop: watch::Sender<bool>) {
            self.0.lock().unwrap().stop_after_send = Some(stop);
        }

        fn cancel_during_send(&self, storage: Storage) {
            self.0.lock().unwrap().cancel_during_send = Some(storage);
        }

        fn cleared(&self) -> Vec<(UserId, MessageId)> {
            self.0.lock().unwrap().cleared.clone()
        }
    }

    impl QuizSender for FakeSender {
        fn send_quiz(
            &self,
            user: UserId,
            _quiz: QuizId,
            term: &str,
            _options: &[String],
        ) -> impl Future<Output = Result<MessageId, SendError>> + Send {
            let mut state = self.0.lock().unwrap();
            let result = match state.failures.get(&user) {
                Some(error) => Err(error.clone()),
                None => {
                    state.sent.push((user, term.to_owned()));
                    if let Some(stop) = &state.stop_after_send {
                        let _ = stop.send(true);
                    }
                    Ok(FAKE_MESSAGE_ID)
                }
            };
            let cancel = state.cancel_during_send.take();
            async move {
                if let Some(storage) = cancel {
                    storage.skip_quiz(user, ts(0)).await.unwrap();
                }
                result
            }
        }

        fn clear_keyboard(
            &self,
            user: UserId,
            message: MessageId,
        ) -> impl Future<Output = ()> + Send {
            self.0.lock().unwrap().cleared.push((user, message));
            future::ready(())
        }
    }

    struct Fixture {
        storage: Storage,
        sender: FakeSender,
        scheduler: Scheduler<FakeSender>,
        collection: CollectionId,
        running: watch::Receiver<bool>,
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_secs(secs)
    }

    fn period() -> Period {
        Period::from_secs(60).unwrap()
    }

    impl Fixture {
        fn rng(&self) -> StdRng {
            StdRng::seed_from_u64(1)
        }

        async fn add_words(&self, count: usize) {
            let existing = self.storage.count_words(self.collection).await.unwrap() as usize;
            let pairs: Vec<ParsedPair> = (existing..existing + count)
                .map(|number| ParsedPair {
                    term: format!("term{number}"),
                    translation: format!("перевод{number}"),
                })
                .collect();
            self.storage
                .import_pairs(self.collection, &pairs)
                .await
                .unwrap();
        }
    }

    /// Users `1..=users` share one collection holding `words` words.
    async fn fixture_with_words(users: i64, words: usize) -> Fixture {
        let storage = Storage::in_memory().await;
        let code: Code = "K7QM-3XPA".parse().unwrap();
        storage
            .ensure_user(UserId(1), period(), ts(0))
            .await
            .unwrap();
        let collection = storage
            .create_collection(UserId(1), "A", &code, ts(0))
            .await
            .unwrap()
            .unwrap()
            .id;
        for id in 2..=users {
            storage
                .ensure_user(UserId(id), period(), ts(0))
                .await
                .unwrap();
            storage
                .join_collection(UserId(id), &code, ts(0))
                .await
                .unwrap();
        }
        let sender = FakeSender::default();
        let service = QuizService::new(
            storage.clone(),
            sender.clone(),
            Arc::new(Config::for_tests()),
        );
        let fixture = Fixture {
            storage,
            sender,
            scheduler: Scheduler::new(service),
            collection,
            running: watch::channel(false).1,
        };
        fixture.add_words(words).await;
        fixture
    }

    #[tokio::test]
    async fn sends_due_quiz_and_stores_message() {
        let fixture = fixture_with_words(1, 4).await;
        assert_eq!(
            fixture
                .scheduler
                .tick(ts(100), &mut fixture.rng(), &fixture.running)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            fixture
                .storage
                .open_quiz(UserId(1))
                .await
                .unwrap()
                .unwrap()
                .message_id,
            Some(FAKE_MESSAGE_ID)
        );
    }

    #[tokio::test]
    async fn waits_for_answer_before_next_quiz() {
        let fixture = fixture_with_words(1, 4).await;
        fixture
            .scheduler
            .tick(ts(100), &mut fixture.rng(), &fixture.running)
            .await
            .unwrap();
        assert_eq!(
            fixture
                .scheduler
                .tick(ts(100_000), &mut fixture.rng(), &fixture.running)
                .await
                .unwrap(),
            0
        );
        assert_eq!(fixture.sender.sent().len(), 1);
    }

    #[tokio::test]
    async fn user_without_words_stays_due_and_does_not_block_others() {
        let fixture = fixture_with_words(1, 4).await;
        for id in 100..400 {
            fixture
                .storage
                .ensure_user(UserId(id), period(), ts(0))
                .await
                .unwrap();
        }
        assert_eq!(
            fixture
                .scheduler
                .tick(ts(100), &mut fixture.rng(), &fixture.running)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            fixture
                .storage
                .get_user(UserId(100))
                .await
                .unwrap()
                .unwrap()
                .next_quiz_at,
            ts(0)
        );
    }

    #[tokio::test]
    async fn quiz_arrives_after_words_appear() {
        let fixture = fixture_with_words(1, 1).await;
        assert_eq!(
            fixture
                .scheduler
                .tick(ts(100), &mut fixture.rng(), &fixture.running)
                .await
                .unwrap(),
            0
        );
        fixture.add_words(4).await;
        assert_eq!(
            fixture
                .scheduler
                .tick(ts(130), &mut fixture.rng(), &fixture.running)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn transient_failure_leaves_no_open_quiz() {
        let fixture = fixture_with_words(1, 4).await;
        fixture.sender.fail(1, SendError::Other("timeout".into()));
        assert_eq!(
            fixture
                .scheduler
                .tick(ts(100), &mut fixture.rng(), &fixture.running)
                .await
                .unwrap(),
            0
        );
        assert!(
            fixture
                .storage
                .open_quiz(UserId(1))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            fixture.storage.due_users(ts(100)).await.unwrap(),
            [UserId(1)]
        );
    }

    #[tokio::test]
    async fn rejected_send_postpones_user_by_one_period() {
        let fixture = fixture_with_words(1, 4).await;
        fixture
            .sender
            .fail(1, SendError::Rejected("message is too long".into()));
        fixture
            .scheduler
            .tick(ts(100), &mut fixture.rng(), &fixture.running)
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
        let fixture = fixture_with_words(1, 4).await;
        fixture.sender.cancel_during_send(fixture.storage.clone());
        fixture
            .scheduler
            .tick(ts(100), &mut fixture.rng(), &fixture.running)
            .await
            .unwrap();
        assert_eq!(fixture.sender.cleared(), [(UserId(1), FAKE_MESSAGE_ID)]);
    }

    #[tokio::test]
    async fn blocked_user_is_paused() {
        let fixture = fixture_with_words(1, 4).await;
        fixture.sender.fail(1, SendError::Blocked);
        fixture
            .scheduler
            .tick(ts(100), &mut fixture.rng(), &fixture.running)
            .await
            .unwrap();
        assert!(
            fixture
                .storage
                .get_user(UserId(1))
                .await
                .unwrap()
                .unwrap()
                .paused
        );
    }

    #[tokio::test]
    async fn one_failure_does_not_stop_others() {
        let fixture = fixture_with_words(2, 4).await;
        fixture.sender.fail(1, SendError::Other("x".into()));
        assert_eq!(
            fixture
                .scheduler
                .tick(ts(100), &mut fixture.rng(), &fixture.running)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn shutdown_between_users_leaves_second_user_due() {
        let fixture = fixture_with_words(2, 4).await;
        let (stop, stopped) = watch::channel(false);
        fixture.sender.stop_after_send(stop);
        assert_eq!(
            fixture
                .scheduler
                .tick(ts(100), &mut fixture.rng(), &stopped)
                .await
                .unwrap(),
            1
        );
        let sent = fixture.sender.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, UserId(1));
        assert!(
            fixture
                .storage
                .open_quiz(UserId(2))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            fixture.storage.due_users(ts(100)).await.unwrap(),
            [UserId(2)]
        );
    }

    #[tokio::test]
    async fn dispatch_due_skips_user_paused_after_listing() {
        let fixture = fixture_with_words(1, 4).await;
        fixture
            .storage
            .set_paused(UserId(1), true, ts(50))
            .await
            .unwrap();
        let outcome = fixture
            .scheduler
            .service
            .dispatch_due(UserId(1), ts(100), &mut fixture.rng())
            .await
            .unwrap();
        assert_eq!(outcome, None);
        assert!(fixture.sender.sent().is_empty());
    }

    #[tokio::test]
    async fn dispatch_due_skips_user_rescheduled_after_listing() {
        let fixture = fixture_with_words(1, 4).await;
        fixture
            .storage
            .set_period(UserId(1), Period::from_secs(600).unwrap(), ts(50))
            .await
            .unwrap();
        let outcome = fixture
            .scheduler
            .service
            .dispatch_due(UserId(1), ts(100), &mut fixture.rng())
            .await
            .unwrap();
        assert_eq!(outcome, None);
        assert!(fixture.sender.sent().is_empty());
    }

    #[tokio::test]
    async fn manual_dispatch_ignores_pause() {
        let fixture = fixture_with_words(1, 4).await;
        fixture
            .storage
            .set_paused(UserId(1), true, ts(50))
            .await
            .unwrap();
        let outcome = fixture
            .scheduler
            .service
            .dispatch(UserId(1), ts(100), &mut fixture.rng())
            .await
            .unwrap();
        assert_eq!(outcome, DispatchOutcome::Sent);
    }

    #[tokio::test]
    async fn eligibility_follows_word_count() {
        let fixture = fixture_with_words(1, 2).await;
        assert!(
            !fixture
                .scheduler
                .service
                .has_eligible_words(UserId(1), ts(100))
                .await
                .unwrap()
        );
        fixture.add_words(2).await;
        assert!(
            fixture
                .scheduler
                .service
                .has_eligible_words(UserId(1), ts(100))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn run_stops_on_shutdown_signal() {
        let fixture = fixture_with_words(1, 4).await;
        let (stop, stopped) = watch::channel(false);
        let handle = tokio::spawn(fixture.scheduler.run(stopped));
        stop.send(true).unwrap();
        time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run did not stop")
            .unwrap();
    }

    #[tokio::test]
    async fn run_exits_when_already_shut_down() {
        let fixture = fixture_with_words(1, 4).await;
        let (_stop, stopped) = watch::channel(true);
        time::timeout(Duration::from_secs(5), fixture.scheduler.run(stopped))
            .await
            .expect("run did not exit");
        assert!(fixture.sender.sent().is_empty());
    }

    #[tokio::test]
    async fn repeated_translations_give_no_eligible_words() {
        let fixture = fixture_with_words(1, 0).await;
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
        let service = &fixture.scheduler.service;
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
    async fn passed_precheck_still_needs_a_fitting_word() {
        // Three distinct translations, but all on one word: they are all
        // excluded as synonyms, so no distractors remain.
        let fixture = fixture_with_words(1, 0).await;
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
                .scheduler
                .service
                .has_eligible_words(UserId(1), ts(100))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn request_resends_open_quiz_and_clears_old_buttons() {
        let fixture = fixture_with_words(1, 4).await;
        let service = &fixture.scheduler.service;
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
        let fixture = fixture_with_words(1, 4).await;
        let service = &fixture.scheduler.service;
        service
            .request_quiz(UserId(1), ts(100), &mut fixture.rng())
            .await
            .unwrap();
        fixture.sender.fail(1, SendError::Blocked);
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
