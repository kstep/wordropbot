pub mod callback;
pub mod command;
pub mod gate;
mod handlers;
pub mod limiter;
pub mod texts;

use std::{convert::Infallible, sync::Arc};

use teloxide::{
    RequestError,
    adaptors::{Throttle, throttle::Limits},
    dispatching::ShutdownToken,
    errors::ApiError,
    prelude::*,
    types::{InlineKeyboardMarkup, MessageId as TgMessageId},
    update_listeners,
};

use crate::{
    bot::{callback::CallbackAction, handlers::Handlers},
    config::Config,
    ids::{MessageId, QuizId, UserId},
    scheduler::Scheduler,
    service::{QuizSender, QuizService, SendError},
    storage::Storage,
};

const FORBIDDEN_PREFIX: &str = "Forbidden";

/// The bot behind Telegram's rate limits.
pub type TelegramBot = Throttle<Bot>;

impl From<RequestError> for SendError {
    fn from(error: RequestError) -> SendError {
        let RequestError::Api(api_error) = &error else {
            return SendError::Other(error.to_string());
        };
        match api_error {
            ApiError::CantInitiateConversation | ApiError::CantTalkWithBots => SendError::Blocked,
            ApiError::MessageTextIsEmpty
            | ApiError::MessageIsTooLong
            | ApiError::ButtonDataInvalid
            | ApiError::ChatNotFound
            | ApiError::UserNotFound => SendError::Rejected(error.to_string()),
            // Forbidden responses without their own variant arrive as
            // `Unknown`.
            ApiError::Unknown(text) if text.starts_with(FORBIDDEN_PREFIX) => SendError::Blocked,
            known if known.to_string().starts_with(FORBIDDEN_PREFIX) => SendError::Blocked,
            _ => SendError::Other(error.to_string()),
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
            [CallbackAction::Answer { quiz, index }.button(option.as_str())]
        }))
    }

    async fn send(
        &self,
        user: UserId,
        quiz: QuizId,
        term: &str,
        options: &[String],
    ) -> Result<MessageId, RequestError> {
        let sent = self
            .bot
            .send_message(ChatId::from(user), term.to_owned())
            .reply_markup(TelegramSender::keyboard(quiz, options))
            .await?;
        Ok(MessageId(sent.id.0))
    }
}

impl QuizSender for TelegramSender {
    async fn send_quiz(
        &self,
        user: UserId,
        quiz: QuizId,
        term: &str,
        options: &[String],
    ) -> Result<MessageId, SendError> {
        Ok(self.send(user, quiz, term, options).await?)
    }

    async fn clear_keyboard(&self, user: UserId, message: MessageId) {
        let _ = self
            .bot
            .edit_message_reply_markup(ChatId::from(user), TgMessageId(message.0))
            .await;
    }
}

/// The Telegram update dispatcher together with everything it needs.
pub struct BotApp {
    bot: TelegramBot,
    dispatcher: Dispatcher<TelegramBot, Infallible, ()>,
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
                    .filter(|message: Message| message.chat.is_private())
                    .endpoint(|message: Message, handlers: Arc<Handlers>| async move {
                        handlers.on_message(message).await;
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
            // Per-chat workers have bounded queues, and one full queue stalls the
            // dispatcher for everyone; `UserGate` keeps each user's order instead.
            .distribution_function(|_| None::<()>)
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

#[cfg(test)]
mod tests {
    use teloxide::types::Seconds;

    use super::*;

    #[test]
    fn telegram_errors_map_to_send_errors() {
        for api_error in [
            ApiError::BotBlocked,
            ApiError::UserDeactivated,
            ApiError::CantInitiateConversation,
            ApiError::CantTalkWithBots,
            ApiError::Unknown("Forbidden: bot is not allowed here".into()),
        ] {
            let error = RequestError::Api(api_error);
            assert_eq!(SendError::from(error), SendError::Blocked);
        }
        for api_error in [
            ApiError::MessageTextIsEmpty,
            ApiError::MessageIsTooLong,
            ApiError::ButtonDataInvalid,
            ApiError::ChatNotFound,
            ApiError::UserNotFound,
        ] {
            let error = RequestError::Api(api_error);
            assert!(matches!(SendError::from(error), SendError::Rejected(_)));
        }
        let transient = [
            RequestError::Api(ApiError::Unknown("Internal Server Error".into())),
            RequestError::RetryAfter(Seconds::from_seconds(5)),
        ];
        for error in transient {
            assert!(matches!(SendError::from(error), SendError::Other(_)));
        }
    }
}
