pub mod callback;
pub mod command;
mod handlers;
pub mod limiter;
pub mod texts;

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;

use teloxide::RequestError;
use teloxide::adaptors::throttle::Limits;
use teloxide::dispatching::{DefaultKey, ShutdownToken};
use teloxide::errors::ApiError;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use teloxide::update_listeners;

use crate::bot::callback::CallbackAction;
use crate::bot::handlers::Handlers;
use crate::config::Config;
use crate::ids::{MessageId, QuizId, UserId};
use crate::scheduler::Scheduler;
use crate::service::{QuizSender, QuizService, SendError};
use crate::storage::Storage;

/// The bot behind Telegram's rate limits.
pub type TelegramBot = teloxide::adaptors::Throttle<Bot>;

impl From<RequestError> for SendError {
    fn from(error: RequestError) -> SendError {
        match error {
            RequestError::Api(ApiError::BotBlocked | ApiError::UserDeactivated) => {
                SendError::Blocked
            }
            other => SendError::Other(other.to_string()),
        }
    }
}

/// In private chats the chat id equals the user id.
impl From<UserId> for ChatId {
    fn from(user: UserId) -> ChatId {
        ChatId(user.0)
    }
}

/// Sends quizzes to private chats, where the chat id equals the user id.
#[derive(Clone)]
pub struct TelegramSender {
    bot: TelegramBot,
}

impl TelegramSender {
    pub fn new(bot: TelegramBot) -> TelegramSender {
        TelegramSender { bot }
    }

    /// One option per row.
    pub fn keyboard(quiz: QuizId, options: &[String]) -> InlineKeyboardMarkup {
        InlineKeyboardMarkup::new(options.iter().enumerate().map(|(index, option)| {
            let action = CallbackAction::Answer { quiz, index };
            [InlineKeyboardButton::callback(
                option.clone(),
                action.to_string(),
            )]
        }))
    }
}

impl TelegramSender {
    /// Sends the quiz message; shared by the scheduler path and `/quiz` resends.
    pub fn send(
        &self,
        user: UserId,
        quiz: QuizId,
        term: &str,
        options: &[String],
    ) -> impl Future<Output = Result<MessageId, RequestError>> + Send {
        let request = self
            .bot
            .send_message(ChatId::from(user), term.to_owned())
            .reply_markup(TelegramSender::keyboard(quiz, options));
        async move { Ok(MessageId(request.await?.id.0)) }
    }
}

impl QuizSender for TelegramSender {
    fn send_quiz(
        &self,
        user: UserId,
        quiz: QuizId,
        term: &str,
        options: &[String],
    ) -> impl Future<Output = Result<MessageId, SendError>> + Send {
        let sent = self.send(user, quiz, term, options);
        async move { Ok(sent.await?) }
    }
}

/// The Telegram update dispatcher together with everything it needs.
pub struct BotApp {
    bot: TelegramBot,
    dispatcher: Dispatcher<TelegramBot, Infallible, DefaultKey>,
    sender: TelegramSender,
    storage: Storage,
    config: Arc<Config>,
}

impl BotApp {
    pub fn new(storage: Storage, config: Arc<Config>) -> BotApp {
        let bot = Bot::new(&config.telegram.token).throttle(Limits::default());
        let sender = TelegramSender::new(bot.clone());
        let handlers = Arc::new(Handlers::new(bot.clone(), storage.clone(), config.clone()));
        let schema = dptree::entry()
            .branch(
                Update::filter_message()
                    .filter(|msg: Message| msg.chat.is_private())
                    .endpoint(|msg: Message, handlers: Arc<Handlers>| async move {
                        handlers.on_message(msg).await;
                        Ok::<(), Infallible>(())
                    }),
            )
            .branch(Update::filter_callback_query().endpoint(
                |query: CallbackQuery, handlers: Arc<Handlers>| async move {
                    handlers.on_callback(query).await;
                    Ok::<(), Infallible>(())
                },
            ));
        let dispatcher = Dispatcher::builder(bot.clone(), schema)
            .dependencies(dptree::deps![handlers])
            // The default handler logs the whole update; unhandled kinds are simply ignored.
            .default_handler(|_| async {})
            .build();
        BotApp {
            bot,
            dispatcher,
            sender,
            storage,
            config,
        }
    }

    pub fn scheduler(&self) -> Scheduler<TelegramSender> {
        Scheduler::new(QuizService::new(
            self.storage.clone(),
            self.sender.clone(),
            self.config.clone(),
        ))
    }

    pub fn shutdown_token(&self) -> ShutdownToken {
        self.dispatcher.shutdown_token()
    }

    /// Fails if Telegram cannot be reached at start (bad token, no network).
    pub async fn run(mut self) -> Result<(), RequestError> {
        let listener = update_listeners::polling_default(self.bot.clone()).await;
        let errors = LoggingErrorHandler::with_custom_text("An error from the update listener");
        self.dispatcher
            .try_dispatch_with_listener(listener, errors)
            .await
    }
}
