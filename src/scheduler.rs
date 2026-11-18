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
        random_generator: &mut StdRng,
        shutdown: &watch::Receiver<bool>,
    ) -> Result<usize, sqlx::Error> {
        let mut sent = 0;
        for user in self.service.due_users(now).await? {
            // A long walk over users must not delay shutdown.
            if *shutdown.borrow() {
                break;
            }
            match self.service.dispatch_due(user, now, random_generator).await {
                Ok(Some(DispatchOutcome::Sent)) => sent += 1,
                Ok(_) => {}
                Err(error) => tracing::warn!(%user, %error, "quiz dispatch failed"),
            }
        }
        Ok(sent)
    }

    /// Ticks until `shutdown` turns true or its sender is dropped.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let mut random_generator = StdRng::from_os_rng();
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
                    if let Err(error) = self.tick(Timestamp::now(), &mut random_generator, &shutdown).await {
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
    use std::{ops::Deref, time::Duration};

    use tokio::task;

    use super::*;
    use crate::{
        ids::UserId,
        service::SendError,
        testing::{FAKE_MESSAGE_ID, FakeSender, QuizFixture, add_user, next_quiz_at, timestamp},
    };

    struct Fixture {
        quiz: QuizFixture,
        scheduler: Scheduler<FakeSender>,
        running: watch::Receiver<bool>,
    }

    impl Deref for Fixture {
        type Target = QuizFixture;

        fn deref(&self) -> &QuizFixture {
            &self.quiz
        }
    }

    /// Users `1..=users` share one collection holding `words` words.
    async fn fixture_with_words(users: i64, words: usize) -> Fixture {
        let quiz = QuizFixture::with_words(users, words).await;
        Fixture {
            scheduler: Scheduler::new(quiz.service()),
            running: watch::channel(false).1,
            quiz,
        }
    }

    #[tokio::test]
    async fn sends_due_quiz_and_stores_message() {
        let fixture = fixture_with_words(1, 4).await;
        assert_eq!(
            fixture
                .scheduler
                .tick(
                    timestamp(100),
                    &mut fixture.random_generator(),
                    &fixture.running
                )
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
            .tick(
                timestamp(100),
                &mut fixture.random_generator(),
                &fixture.running,
            )
            .await
            .unwrap();
        assert_eq!(
            fixture
                .scheduler
                .tick(
                    timestamp(100_000),
                    &mut fixture.random_generator(),
                    &fixture.running
                )
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
            add_user(&fixture.storage, UserId(id)).await;
        }
        assert_eq!(
            fixture
                .scheduler
                .tick(
                    timestamp(100),
                    &mut fixture.random_generator(),
                    &fixture.running
                )
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            next_quiz_at(&fixture.storage, UserId(100)).await,
            timestamp(0)
        );
    }

    #[tokio::test]
    async fn quiz_arrives_after_words_appear() {
        let fixture = fixture_with_words(1, 1).await;
        assert_eq!(
            fixture
                .scheduler
                .tick(
                    timestamp(100),
                    &mut fixture.random_generator(),
                    &fixture.running
                )
                .await
                .unwrap(),
            0
        );
        fixture.add_words(4).await;
        assert_eq!(
            fixture
                .scheduler
                .tick(
                    timestamp(130),
                    &mut fixture.random_generator(),
                    &fixture.running
                )
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn transient_failure_leaves_no_open_quiz() {
        let fixture = fixture_with_words(1, 4).await;
        fixture
            .sender
            .fail(UserId(1), SendError::Other("timeout".into()));
        assert_eq!(
            fixture
                .scheduler
                .tick(
                    timestamp(100),
                    &mut fixture.random_generator(),
                    &fixture.running
                )
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
            fixture.storage.due_users(timestamp(100)).await.unwrap(),
            [UserId(1)]
        );
    }

    #[tokio::test]
    async fn blocked_user_is_paused() {
        let fixture = fixture_with_words(1, 4).await;
        fixture.sender.fail(UserId(1), SendError::Blocked);
        fixture
            .scheduler
            .tick(
                timestamp(100),
                &mut fixture.random_generator(),
                &fixture.running,
            )
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
        fixture.sender.fail(UserId(1), SendError::Other("x".into()));
        assert_eq!(
            fixture
                .scheduler
                .tick(
                    timestamp(100),
                    &mut fixture.random_generator(),
                    &fixture.running
                )
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
                .tick(timestamp(100), &mut fixture.random_generator(), &stopped)
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
            fixture.storage.due_users(timestamp(100)).await.unwrap(),
            [UserId(2)]
        );
    }

    #[tokio::test]
    async fn run_stops_on_shutdown_signal() {
        for stopped_before_start in [true, false] {
            let fixture = fixture_with_words(1, 4).await;
            let sender = fixture.sender.clone();
            let (stop, stopped) = watch::channel(stopped_before_start);
            let handle = tokio::spawn(fixture.scheduler.run(stopped));
            if !stopped_before_start {
                // Lets `run` reach its select, so the signal arrives through
                // `changed()`.
                task::yield_now().await;
                stop.send(true).unwrap();
            }
            time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("run did not stop")
                .unwrap();
            if stopped_before_start {
                assert!(sender.sent().is_empty());
            }
        }
    }
}
