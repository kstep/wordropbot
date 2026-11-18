//! Helpers shared by the unit tests.

use std::{
    collections::HashMap,
    future::{self, Future},
    sync::{Arc, Mutex},
};

use rand::{SeedableRng, rngs::StdRng};
use tokio::sync::watch;

use crate::{
    code::Code,
    config::Config,
    ids::{CollectionId, MessageId, QuizId, UserId},
    import::ParsedPair,
    period::Period,
    service::{QuizSender, QuizService, SendError},
    storage::Storage,
    time::Timestamp,
};

pub const FAKE_MESSAGE_ID: MessageId = MessageId(777);
const FIXTURE_CODE: &str = "K7QM-3XPA";

pub fn timestamp(seconds: i64) -> Timestamp {
    Timestamp::from_secs(seconds)
}

pub fn user_id(id: i64) -> UserId {
    UserId(id)
}

pub fn message_id(id: i32) -> MessageId {
    MessageId(id)
}

pub fn pair(term: &str, translation: &str) -> ParsedPair {
    ParsedPair {
        term: term.into(),
        translation: translation.into(),
    }
}

pub fn minute_period() -> Period {
    Period::from_secs(60).unwrap()
}

/// Creates the user with a one-minute period, due from time zero.
pub async fn add_user(storage: &Storage, user: UserId) {
    storage
        .ensure_user(user, minute_period(), timestamp(0))
        .await
        .unwrap();
}

/// Creates the user together with one collection that is active for them.
pub async fn user_with_collection(storage: &Storage, user: UserId, code: &str) -> CollectionId {
    add_user(storage, user).await;
    storage
        .create_collection(user, "A", &code.parse().unwrap(), timestamp(0))
        .await
        .unwrap()
        .unwrap()
        .id
}

pub async fn next_quiz_at(storage: &Storage, user: UserId) -> Timestamp {
    storage.get_user(user).await.unwrap().unwrap().next_quiz_at
}

#[derive(Default)]
struct FakeState {
    sent: Vec<(UserId, String)>,
    cleared: Vec<(UserId, MessageId)>,
    /// Signalled after each successful send, to stop in message_id-walk.
    stop_after_send: Option<watch::Sender<bool>>,
    failures: HashMap<UserId, SendError>,
    /// Skips the user's quiz in the middle of the next send.
    cancel_during_send: Option<Storage>,
}

#[derive(Clone, Default)]
pub struct FakeSender(Arc<Mutex<FakeState>>);

impl FakeSender {
    pub fn fail(&self, user: UserId, error: SendError) {
        self.0.lock().unwrap().failures.insert(user, error);
    }

    pub fn sent(&self) -> Vec<(UserId, String)> {
        self.0.lock().unwrap().sent.clone()
    }

    pub fn cleared(&self) -> Vec<(UserId, MessageId)> {
        self.0.lock().unwrap().cleared.clone()
    }

    pub fn stop_after_send(&self, stop: watch::Sender<bool>) {
        self.0.lock().unwrap().stop_after_send = Some(stop);
    }

    pub fn cancel_during_send(&self, storage: Storage) {
        self.0.lock().unwrap().cancel_during_send = Some(storage);
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
                storage.skip_quiz(user, timestamp(0)).await.unwrap();
            }
            result
        }
    }

    fn clear_keyboard(&self, user: UserId, message: MessageId) -> impl Future<Output = ()> + Send {
        self.0.lock().unwrap().cleared.push((user, message));
        future::ready(())
    }
}

/// Users `1..=users` sharing one collection, with a fake Telegram behind them.
pub struct QuizFixture {
    pub storage: Storage,
    pub sender: FakeSender,
    pub collection: CollectionId,
}

impl QuizFixture {
    pub async fn with_words(users: i64, words: usize) -> QuizFixture {
        let storage = Storage::in_memory().await;
        let collection = user_with_collection(&storage, UserId(1), FIXTURE_CODE).await;
        let code: Code = FIXTURE_CODE.parse().unwrap();
        for id in 2..=users {
            add_user(&storage, UserId(id)).await;
            storage
                .join_collection(UserId(id), &code, timestamp(0))
                .await
                .unwrap();
        }
        let fixture = QuizFixture {
            storage,
            sender: FakeSender::default(),
            collection,
        };
        fixture.add_words(words).await;
        fixture
    }

    pub fn service(&self) -> QuizService<FakeSender> {
        QuizService::new(
            self.storage.clone(),
            self.sender.clone(),
            Arc::new(Config::for_tests()),
        )
    }

    pub fn random_generator(&self) -> StdRng {
        StdRng::seed_from_u64(1)
    }

    pub async fn add_words(&self, count: usize) {
        let existing = self.storage.count_words(self.collection).await.unwrap() as usize;
        let pairs: Vec<ParsedPair> = (existing..existing + count)
            .map(|number| pair(&format!("term{number}"), &format!("перевод{number}")))
            .collect();
        self.storage
            .import_pairs(self.collection, &pairs)
            .await
            .unwrap();
    }
}
