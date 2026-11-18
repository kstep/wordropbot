use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::sync::watch;
use tokio::time::{self, MissedTickBehavior};

use crate::service::{DispatchOutcome, QuizSender, QuizService};
use crate::time::Timestamp;

pub struct Scheduler<S> {
    service: QuizService<S>,
}

impl<S: QuizSender> Scheduler<S> {
    pub fn new(service: QuizService<S>) -> Self {
        Scheduler { service }
    }

    /// Returns the number of quizzes sent; stops walking users once `shutdown` is true.
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
                Ok(DispatchOutcome::Sent) => sent += 1,
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
        // `changed()` only reports later changes, so a shutdown sent before start needs this check.
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
    use super::*;
    use crate::code::Code;
    use crate::config::Config;
    use crate::ids::{CollectionId, MessageId, QuizId, UserId};
    use crate::import::ParsedPair;
    use crate::period::Period;
    use crate::service::SendError;
    use crate::storage::Storage;
    use std::collections::HashMap;
    use std::future::{self, Future};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const FAKE_MESSAGE_ID: MessageId = MessageId(777);

    #[derive(Default)]
    struct FakeState {
        sent: Vec<(UserId, String)>,
        failures: HashMap<UserId, SendError>,
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
                    Ok(FAKE_MESSAGE_ID)
                }
            };
            future::ready(result)
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
                .map(|i| ParsedPair {
                    term: format!("term{i}"),
                    translation: format!("перевод{i}"),
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
    async fn tick_stops_walking_users_after_shutdown() {
        let fixture = fixture_with_words(2, 4).await;
        let (_tx, stopped) = watch::channel(true);
        assert_eq!(
            fixture
                .scheduler
                .tick(ts(100), &mut fixture.rng(), &stopped)
                .await
                .unwrap(),
            0
        );
        assert!(fixture.sender.sent().is_empty());
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
        assert_eq!(outcome, DispatchOutcome::NotDue);
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
        assert_eq!(outcome, DispatchOutcome::NotDue);
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
        let (_tx, stopped) = watch::channel(true);
        time::timeout(Duration::from_secs(5), fixture.scheduler.run(stopped))
            .await
            .expect("run did not exit");
        assert!(fixture.sender.sent().is_empty());
    }
}
