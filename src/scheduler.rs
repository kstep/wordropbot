use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

use crate::service::{DispatchOutcome, QuizSender, QuizService};
use crate::time::Timestamp;

pub struct Scheduler<S> {
    service: QuizService<S>,
}

impl<S: QuizSender> Scheduler<S> {
    pub fn new(service: QuizService<S>) -> Self {
        Scheduler { service }
    }

    /// Returns the number of quizzes sent.
    pub async fn tick(&self, now: Timestamp, rng: &mut StdRng) -> Result<usize, sqlx::Error> {
        let mut sent = 0;
        for user in self.service.due_users(now).await? {
            match self.service.dispatch(user, now, rng).await {
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
        let mut interval = tokio::time::interval(self.service.tick_interval());
        // A slow tick should not be followed by a burst of catch-up ticks.
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(error) = self.tick(Timestamp::now(), &mut rng).await {
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
    use std::future::Future;
    use std::sync::{Arc, Mutex};

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
            std::future::ready(result)
        }
    }

    struct Fixture {
        s: Storage,
        sender: FakeSender,
        scheduler: Scheduler<FakeSender>,
        collection: CollectionId,
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
            let existing = self.s.count_words(self.collection).await.unwrap() as usize;
            let pairs: Vec<ParsedPair> = (existing..existing + count)
                .map(|i| ParsedPair {
                    term: format!("term{i}"),
                    translation: format!("перевод{i}"),
                })
                .collect();
            self.s.import_pairs(self.collection, &pairs).await.unwrap();
        }
    }

    /// Users `1..=users` share one collection holding `words` words.
    async fn fixture_with_words(users: i64, words: usize) -> Fixture {
        let s = Storage::in_memory().await;
        let code: Code = "K7QM-3XPA".parse().unwrap();
        s.ensure_user(UserId(1), period(), ts(0)).await.unwrap();
        let collection = s
            .create_collection(UserId(1), "A", &code, ts(0))
            .await
            .unwrap()
            .unwrap()
            .id;
        for id in 2..=users {
            s.ensure_user(UserId(id), period(), ts(0)).await.unwrap();
            s.join_collection(UserId(id), &code, ts(0)).await.unwrap();
        }
        let sender = FakeSender::default();
        let service = QuizService::new(s.clone(), sender.clone(), Arc::new(Config::for_tests()));
        let f = Fixture {
            s,
            sender,
            scheduler: Scheduler::new(service),
            collection,
        };
        f.add_words(words).await;
        f
    }

    #[tokio::test]
    async fn sends_due_quiz_and_stores_message() {
        let f = fixture_with_words(1, 4).await;
        assert_eq!(f.scheduler.tick(ts(100), &mut f.rng()).await.unwrap(), 1);
        assert_eq!(
            f.s.open_quiz(UserId(1)).await.unwrap().unwrap().message_id,
            Some(FAKE_MESSAGE_ID)
        );
    }

    #[tokio::test]
    async fn waits_for_answer_before_next_quiz() {
        let f = fixture_with_words(1, 4).await;
        f.scheduler.tick(ts(100), &mut f.rng()).await.unwrap();
        assert_eq!(
            f.scheduler.tick(ts(100_000), &mut f.rng()).await.unwrap(),
            0
        );
        assert_eq!(f.sender.sent().len(), 1);
    }

    #[tokio::test]
    async fn user_without_words_stays_due_and_does_not_block_others() {
        let f = fixture_with_words(1, 4).await;
        for id in 100..400 {
            f.s.ensure_user(UserId(id), period(), ts(0)).await.unwrap();
        }
        assert_eq!(f.scheduler.tick(ts(100), &mut f.rng()).await.unwrap(), 1);
        assert_eq!(
            f.s.get_user(UserId(100))
                .await
                .unwrap()
                .unwrap()
                .next_quiz_at,
            ts(0)
        );
    }

    #[tokio::test]
    async fn quiz_arrives_after_words_appear() {
        let f = fixture_with_words(1, 1).await;
        assert_eq!(f.scheduler.tick(ts(100), &mut f.rng()).await.unwrap(), 0);
        f.add_words(4).await;
        assert_eq!(f.scheduler.tick(ts(130), &mut f.rng()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn transient_failure_leaves_no_open_quiz() {
        let f = fixture_with_words(1, 4).await;
        f.sender.fail(1, SendError::Other("timeout".into()));
        assert_eq!(f.scheduler.tick(ts(100), &mut f.rng()).await.unwrap(), 0);
        assert!(f.s.open_quiz(UserId(1)).await.unwrap().is_none());
        assert_eq!(f.s.due_users(ts(100)).await.unwrap(), [UserId(1)]);
    }

    #[tokio::test]
    async fn blocked_user_is_paused() {
        let f = fixture_with_words(1, 4).await;
        f.sender.fail(1, SendError::Blocked);
        f.scheduler.tick(ts(100), &mut f.rng()).await.unwrap();
        assert!(f.s.get_user(UserId(1)).await.unwrap().unwrap().paused);
    }

    #[tokio::test]
    async fn one_failure_does_not_stop_others() {
        let f = fixture_with_words(2, 4).await;
        f.sender.fail(1, SendError::Other("x".into()));
        assert_eq!(f.scheduler.tick(ts(100), &mut f.rng()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn run_stops_on_shutdown_signal() {
        let f = fixture_with_words(1, 4).await;
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(f.scheduler.run(rx));
        tx.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("run did not stop")
            .unwrap();
    }
}
