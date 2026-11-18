use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rand::{SeedableRng, rngs::StdRng};
use teloxide::{
    RequestError,
    errors::ApiError,
    prelude::*,
    types::{CallbackQuery, InlineKeyboardMarkup, Message, MessageId as TgMessageId},
};

use crate::{
    bot::{
        TelegramBot, TelegramSender,
        callback::CallbackAction,
        command::Command,
        limiter::JoinLimiter,
        texts::{
            AnswerResult, Help, ImportReport, NoActiveCollection, NotEnoughWords, PAUSED_NOTE,
            StatsReport, WORDS_PAGE_SIZE, WordsPage,
        },
    },
    code::Code,
    config::Config,
    ids::{CollectionId, QuizId, UserId, WordId},
    import::ParseReport,
    period::Period,
    service::{QuizRequest, QuizService},
    storage::{
        CancelledQuiz, CollectionError, CollectionName, CollectionNameError, CollectionSummary,
        Storage, User,
    },
    time::Timestamp,
};

const APOLOGY: &str = "Что-то пошло не так, попробуй ещё раз позже.";
const IMPORT_FORMAT: &str = "\
Слова присылай по паре в строке: «слово перевод», например:
σπίτι дом
Если у слова или перевода несколько слов, раздели их через « - »:
το σπίτι - дом";
/// A fresh code collides with an existing one with negligible probability.
const CODE_ATTEMPTS: usize = 5;
const BUTTON_NAME_CHARS: usize = 30;
const OPEN_QUIZ_NOTE: &str = "Сейчас открыт квиз: ответь на него, пришли заново через /quiz \
                              или отмени через /skip.";

#[derive(Debug, thiserror::Error)]
enum HandlerError {
    #[error("storage: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("telegram: {0}")]
    Telegram(#[from] RequestError),
    #[error("no free collection code after {CODE_ATTEMPTS} attempts")]
    NoFreeCode,
}

type Reply<T = ()> = Result<T, HandlerError>;
/// Short text shown to the user on top of the screen after a button press.
type Notice = Option<&'static str>;

/// Where a button was pressed.
#[derive(Clone, Copy)]
struct Press {
    user: UserId,
    message: TgMessageId,
}

struct View {
    text: String,
    keyboard: Option<InlineKeyboardMarkup>,
}

impl View {
    /// The collection list with one pair of buttons per collection.
    fn collections(collections: &[CollectionSummary]) -> View {
        let mut text = String::from("Твои коллекции:");
        let mut rows = Vec::new();
        for collection in collections {
            let mark = if collection.is_active {
                " (активная)"
            } else {
                ""
            };
            text.push_str(&format!(
                "\n{} — {}, слов: {}{mark}",
                collection.name, collection.code, collection.word_count
            ));
            let name: String = collection.name.chars().take(BUTTON_NAME_CHARS).collect();
            rows.push(vec![
                CallbackAction::Activate(collection.id).button(format!("Выбрать «{name}»")),
                CallbackAction::LeaveAsk(collection.id).button("Выйти"),
            ]);
        }
        View {
            text,
            keyboard: Some(InlineKeyboardMarkup::new(rows)),
        }
    }

    /// One page of the collection's words; the page is clamped to the valid
    /// range.
    async fn words(
        storage: &Storage,
        collection: CollectionId,
        page: i64,
    ) -> Result<View, sqlx::Error> {
        let total = storage.count_words(collection).await?;
        if total == 0 {
            return Ok(View {
                text: "В коллекции пока нет слов.".into(),
                keyboard: None,
            });
        }
        let total_pages = (total + WORDS_PAGE_SIZE - 1) / WORDS_PAGE_SIZE;
        let page = page.clamp(1, total_pages);
        let words = storage
            .list_words(collection, (page - 1) * WORDS_PAGE_SIZE, WORDS_PAGE_SIZE)
            .await?;
        let mut row = Vec::new();
        if page > 1 {
            row.push(CallbackAction::WordsPage(page - 1).button("◀"));
        }
        if page < total_pages {
            row.push(CallbackAction::WordsPage(page + 1).button("▶"));
        }
        Ok(View {
            text: WordsPage {
                words: &words,
                page,
                total_pages,
            }
            .to_string(),
            keyboard: (!row.is_empty()).then(|| InlineKeyboardMarkup::new([row])),
        })
    }
}

/// One method per command and per callback action.
pub struct Handlers {
    bot: TelegramBot,
    storage: Storage,
    service: QuizService<TelegramSender>,
    config: Arc<Config>,
    limiter: Mutex<JoinLimiter>,
}

impl Handlers {
    pub fn new(bot: TelegramBot, storage: Storage, config: Arc<Config>) -> Handlers {
        let sender = TelegramSender::new(bot.clone());
        let service = QuizService::new(storage.clone(), sender, config.clone());
        let limiter = Mutex::new(JoinLimiter::new(config.limits.join_attempts_per_minute));
        Handlers {
            bot,
            storage,
            service,
            config,
            limiter,
        }
    }

    // Entry points: failures are logged and never reach the dispatcher.

    pub async fn on_message(&self, message: Message) {
        let user = UserId(message.chat.id.0);
        if let Err(error) = self.handle_message(user, message.text()).await {
            tracing::error!(%user, %error, "message handler failed");
            // The apology is best effort: the failure may be Telegram itself.
            let _ = self.say(user, APOLOGY).await;
        }
    }

    pub async fn on_callback(&self, query: CallbackQuery) {
        // `id()` covers inaccessible messages too, so their edits are not lost.
        let press = match (&query.message, i64::try_from(query.from.id.0)) {
            (Some(message), Ok(user)) if message.chat().is_private() => Some(Press {
                user: UserId(user),
                message: message.id(),
            }),
            _ => None,
        };
        let notice = match press {
            Some(press) => self.press_notice(press, query.data.as_deref()).await,
            // Group or inline-mode buttons are not ours; the button still needs
            // an answer.
            None => None,
        };
        let mut answer = self.bot.answer_callback_query(query.id.clone());
        if let Some(text) = notice {
            answer = answer.text(text);
        }
        if let Err(error) = answer.await {
            tracing::warn!(%error, "answering callback failed");
        }
    }

    async fn press_notice(&self, press: Press, data: Option<&str>) -> Notice {
        self.handle_callback(press, data)
            .await
            .unwrap_or_else(|error| {
                tracing::error!(user = %press.user, %error, "callback handler failed");
                Some(APOLOGY)
            })
    }

    async fn handle_message(&self, user_id: UserId, text: Option<&str>) -> Reply {
        let user = self.ensure_user(user_id).await?;
        let Some(text) = text else { return Ok(()) };
        let command: Result<Command, _> = text.parse();
        match command {
            Ok(Command::Start) => self.help(user.id, user.paused).await,
            Ok(Command::Help | Command::Unknown) => self.help(user.id, false).await,
            Ok(Command::CollectionList) => self.collection_list(user.id).await,
            Ok(Command::CollectionNew(name)) => self.collection_new(user.id, &name).await,
            Ok(Command::CollectionJoin(raw)) => self.collection_join(user.id, &raw).await,
            Ok(Command::Words) => self.words(user).await,
            Ok(Command::Remove(term)) => self.remove(user, &term).await,
            Ok(Command::Stats) => self.stats(user).await,
            Ok(Command::Period(raw)) => self.period(user.id, &raw).await,
            Ok(Command::Pause) => self.pause(user.id).await,
            Ok(Command::Resume) => self.resume(user.id).await,
            Ok(Command::Quiz) => self.quiz(user.id).await,
            Ok(Command::Skip) => self.skip(user.id).await,
            Err(_) => self.import_text(user, text).await,
        }
    }

    async fn handle_callback(&self, press: Press, data: Option<&str>) -> Reply<Notice> {
        let user = self.ensure_user(press.user).await?;
        let action: Option<CallbackAction> = data.and_then(|data| data.parse().ok());
        let Some(action) = action else {
            return Ok(Some("Эта кнопка больше не работает."));
        };
        match action {
            CallbackAction::Answer { quiz, index } => self.answer(press, quiz, index).await,
            CallbackAction::Activate(id) => self.activate(press, id).await,
            CallbackAction::LeaveAsk(id) => self.leave_ask(press, id).await,
            CallbackAction::LeaveConfirm(id) => self.leave_confirm(press, id).await,
            CallbackAction::RemoveConfirm(id) => self.remove_confirm(press, id).await,
            CallbackAction::WordsPage(page) => self.words_page(press, user, page).await,
            CallbackAction::Dismiss => self.dismiss(press).await,
        }
    }

    // Commands.

    async fn help(&self, user: UserId, paused: bool) -> Reply {
        let help = Help {
            quiz_options: self.config.quiz.options,
            paused,
        };
        self.say(user, help.to_string()).await
    }

    async fn collection_new(&self, user: UserId, name: &str) -> Reply {
        let max = self.config.limits.max_term_chars;
        let name = match CollectionName::new(name, max) {
            Ok(name) => name,
            Err(CollectionNameError::Empty) => {
                return self.say(user, "Название не может быть пустым.").await;
            }
            Err(CollectionNameError::TooLong { max }) => {
                return self
                    .say(
                        user,
                        format!("Название слишком длинное: не больше {max} символов."),
                    )
                    .await;
            }
        };
        for _ in 0..CODE_ATTEMPTS {
            let code = Code::generate(&mut rand::rng());
            let created = self
                .storage
                .create_collection(user, name.as_str(), &code, Timestamp::now())
                .await;
            let collection = match created {
                Ok(Some(collection)) => collection,
                // The code is taken: try another one.
                Ok(None) => continue,
                Err(CollectionError::LimitReached { max }) => {
                    return self.say(user, Self::collection_limit_text(max)).await;
                }
                Err(CollectionError::Storage(error)) => return Err(error.into()),
            };
            let text = format!(
                "Коллекция «{}» создана и выбрана активной.\nКод для подключения: {}\n\
                 Теперь отправь слова обычным сообщением.",
                collection.name, collection.code
            );
            // The collection is saved: the apology path would invite a retry
            // that creates a second one.
            if let Err(error) = self.say(user, text).await {
                tracing::warn!(%user, %error, "reporting created collection failed");
            }
            return Ok(());
        }
        Err(HandlerError::NoFreeCode)
    }

    async fn collection_join(&self, user: UserId, raw: &str) -> Reply {
        if !self.limiter().allow(user, Timestamp::now()) {
            return self
                .say(
                    user,
                    "Слишком много неверных попыток. Подожди минуту и попробуй снова.",
                )
                .await;
        }
        let Ok(code): Result<Code, _> = raw.parse() else {
            self.limiter().record_failure(user, Timestamp::now());
            return self
                .say(
                    user,
                    "Код выглядит неверно: нужно 8 символов, например K7QM-3XPA.",
                )
                .await;
        };
        match self
            .storage
            .join_collection(user, &code, Timestamp::now())
            .await
        {
            Ok(Some(collection)) => {
                let text = format!("Ты в коллекции «{}», она теперь активная.", collection.name);
                self.say(user, text).await
            }
            Ok(None) => {
                self.limiter().record_failure(user, Timestamp::now());
                self.say(user, "Коллекции с таким кодом нет. Проверь код.")
                    .await
            }
            Err(CollectionError::LimitReached { max }) => {
                self.say(user, Self::collection_limit_text(max)).await
            }
            Err(CollectionError::Storage(error)) => Err(error.into()),
        }
    }

    async fn collection_list(&self, user: UserId) -> Reply {
        let collections = self.storage.list_collections(user).await?;
        if collections.is_empty() {
            return self.no_collection(user).await;
        }
        let view = View::collections(&collections);
        self.say_with(user, view.text, view.keyboard).await
    }

    async fn import_text(&self, user: User, text: &str) -> Reply {
        let Some(collection) = self.require_collection(user).await? else {
            return Ok(());
        };
        let report = ParseReport::parse(text, &self.config.limits);
        let summary = self.storage.import_pairs(collection, &report.pairs).await?;
        let mut reply = ImportReport {
            summary: &summary,
            bad_lines: &report.bad_lines,
            limits: &self.config.limits,
        }
        .to_string();
        if report.pairs.is_empty() {
            reply.push_str("\n\n");
            reply.push_str(IMPORT_FORMAT);
        } else {
            // The import is saved, so a failed check only costs the hint.
            match self
                .service
                .has_eligible_words(user.id, Timestamp::now())
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    let hint = NotEnoughWords {
                        quiz_options: self.config.quiz.options,
                    };
                    reply.push_str(&format!("\n\n{hint}"));
                }
                Err(error) => {
                    tracing::warn!(user = %user.id, %error, "checking eligible words after import failed");
                }
            }
        }
        self.say(user.id, reply).await
    }

    async fn words(&self, user: User) -> Reply {
        let Some(collection) = self.require_collection(user).await? else {
            return Ok(());
        };
        let view = View::words(&self.storage, collection, 1).await?;
        self.say_with(user.id, view.text, view.keyboard).await
    }

    async fn remove(&self, user: User, term: &str) -> Reply {
        let Some(collection) = self.require_collection(user).await? else {
            return Ok(());
        };
        let Some(word) = self.storage.find_word(collection, term).await? else {
            return self
                .say(user.id, "Такого слова нет в активной коллекции.")
                .await;
        };
        let text = format!(
            "Удалить «{}»? Слово исчезнет у всех участников коллекции вместе с историей ответов.",
            word.term
        );
        let keyboard = InlineKeyboardMarkup::new([[
            CallbackAction::RemoveConfirm(word.id).button("Удалить"),
            CallbackAction::Dismiss.button("Отмена"),
        ]]);
        self.say_with(user.id, text, Some(keyboard)).await
    }

    async fn stats(&self, user: User) -> Reply {
        let Some(collection) = self.require_collection(user).await? else {
            return Ok(());
        };
        let stats = self
            .storage
            .collection_stats(
                user.id,
                collection,
                Timestamp::now(),
                &self.config.selection,
            )
            .await?;
        self.say(user.id, StatsReport(&stats).to_string()).await
    }

    async fn period(&self, user: UserId, raw: &str) -> Reply {
        let Ok(period): Result<Period, _> = raw.parse() else {
            return self
                .say(user, "Не понял период. Примеры: 30m, 2h, 1d.")
                .await;
        };
        let min = self.config.schedule.min_period;
        if period < min {
            return self
                .say(user, format!("Период не может быть короче {min}."))
                .await;
        }
        let quiz_open = self
            .storage
            .set_period(user, period, Timestamp::now())
            .await?;
        let text = Self::with_open_quiz_note(format!("Период квизов: {period}."), quiz_open);
        self.say(user, text).await
    }

    async fn pause(&self, user: UserId) -> Reply {
        self.storage
            .set_paused(user, true, Timestamp::now())
            .await?;
        self.say(user, PAUSED_NOTE).await
    }

    async fn resume(&self, user: UserId) -> Reply {
        let quiz_open = self
            .storage
            .set_paused(user, false, Timestamp::now())
            .await?;
        let text = Self::with_open_quiz_note("Квизы возобновлены.".into(), quiz_open);
        self.say(user, text).await
    }

    async fn quiz(&self, user: UserId) -> Reply {
        let mut rng = StdRng::from_os_rng();
        match self
            .service
            .request_quiz(user, Timestamp::now(), &mut rng)
            .await?
        {
            QuizRequest::Sent | QuizRequest::Blocked => Ok(()),
            QuizRequest::Raced => self.say(user, "Квиз уже открыт, повтори /quiz.").await,
            QuizRequest::NoEligibleWords => {
                self.say(
                    user,
                    "Пока недостаточно слов для квиза: добавь ещё слов в активную коллекцию.",
                )
                .await
            }
            QuizRequest::Failed => self.say(user, APOLOGY).await,
        }
    }

    async fn skip(&self, user: UserId) -> Reply {
        match self.storage.skip_quiz(user, Timestamp::now()).await? {
            Some(cancelled) => {
                self.drop_keyboards(&[cancelled]).await;
                self.say(user, "Квиз пропущен, ошибкой это не считается.")
                    .await
            }
            None => self.say(user, "Открытого квиза нет.").await,
        }
    }

    // Button presses.

    async fn answer(&self, press: Press, quiz: QuizId, index: usize) -> Reply<Notice> {
        let outcome = self
            .storage
            .answer_quiz(quiz, press.user, index, Timestamp::now())
            .await?;
        match AnswerResult::new(&outcome) {
            Some(result) => Ok(self.show_committed(press, result.to_string()).await),
            None => Ok(Some("Этот квиз уже закрыт.")),
        }
    }

    async fn activate(&self, press: Press, id: CollectionId) -> Reply<Notice> {
        if !self.storage.set_active_collection(press.user, id).await? {
            return Ok(Some("Ты не участник этой коллекции."));
        }
        // Refresh the "(активная)" mark. The choice is saved, so a failed
        // refresh is not an error.
        if let Err(error) = self.refresh_collections(press).await {
            tracing::warn!(user = %press.user, %error, "refreshing collection list failed");
        }
        Ok(Some("Коллекция выбрана."))
    }

    async fn refresh_collections(&self, press: Press) -> Reply {
        let collections = self.storage.list_collections(press.user).await?;
        let view = View::collections(&collections);
        self.edit(press, view.text, view.keyboard).await
    }

    async fn leave_ask(&self, press: Press, id: CollectionId) -> Reply<Notice> {
        let collections = self.storage.list_collections(press.user).await?;
        let Some(collection) = collections.iter().find(|listed| listed.id == id) else {
            return Ok(Some("Ты не участник этой коллекции."));
        };
        let text = format!(
            "Выйти из коллекции «{}»? История ответов сохранится, открытый квиз из неё отменится. \
             Если ты последний участник, коллекция удалится со всеми словами.",
            collection.name
        );
        let keyboard = InlineKeyboardMarkup::new([[
            CallbackAction::LeaveConfirm(id).button("Выйти"),
            CallbackAction::Dismiss.button("Отмена"),
        ]]);
        self.bot
            .send_message(ChatId::from(press.user), text)
            .reply_markup(keyboard)
            .await?;
        Ok(None)
    }

    async fn leave_confirm(&self, press: Press, id: CollectionId) -> Reply<Notice> {
        let Some(cancelled) = self
            .storage
            .leave_collection(press.user, id, Timestamp::now())
            .await?
        else {
            return Ok(Some("Ты не участник этой коллекции."));
        };
        self.drop_keyboards(&cancelled).await;
        Ok(self
            .show_committed(press, "Ты вышел из коллекции.".into())
            .await)
    }

    async fn remove_confirm(&self, press: Press, id: WordId) -> Reply<Notice> {
        let Some(cancelled) = self
            .storage
            .remove_word(press.user, id, Timestamp::now())
            .await?
        else {
            return Ok(Some("Слово уже удалено или недоступно."));
        };
        self.drop_keyboards(&cancelled).await;
        Ok(self.show_committed(press, "Слово удалено.".into()).await)
    }

    async fn words_page(&self, press: Press, user: User, page: i64) -> Reply<Notice> {
        // The active collection always belongs to the user: leaving clears it
        // in the same transaction as the membership.
        let Some(collection) = user.active_collection_id else {
            return Ok(Some("Активной коллекции нет."));
        };
        let view = View::words(&self.storage, collection, page).await?;
        self.edit(press, view.text, view.keyboard).await?;
        Ok(None)
    }

    async fn dismiss(&self, press: Press) -> Reply<Notice> {
        // Deleting is cosmetic: a failure only leaves the stale buttons in
        // place.
        let _ = self
            .bot
            .delete_message(ChatId::from(press.user), press.message)
            .await;
        Ok(Some("Отменено."))
    }

    // Helpers.

    async fn ensure_user(&self, id: UserId) -> Result<User, sqlx::Error> {
        self.storage
            .ensure_user(id, self.config.schedule.default_period, Timestamp::now())
            .await
    }

    /// The user's active collection. With none, tells the user to pick one and
    /// returns `None`.
    async fn require_collection(&self, user: User) -> Reply<Option<CollectionId>> {
        let collection = user.active_collection_id;
        if collection.is_none() {
            self.no_collection(user.id).await?;
        }
        Ok(collection)
    }

    /// Tells the user an active collection is needed, hinting at `/collection`
    /// when they already have collections to pick from.
    async fn no_collection(&self, user: UserId) -> Reply {
        let has_collections = !self.storage.list_collections(user).await?.is_empty();
        self.say(user, NoActiveCollection { has_collections }.to_string())
            .await
    }

    fn collection_limit_text(max: usize) -> String {
        format!("У тебя уже максимум коллекций ({max}). Выйди из ненужной через /collection.")
    }

    fn with_open_quiz_note(mut text: String, quiz_open: bool) -> String {
        if quiz_open {
            text.push('\n');
            text.push_str(OPEN_QUIZ_NOTE);
        }
        text
    }

    fn limiter(&self) -> MutexGuard<'_, JoinLimiter> {
        // The limiter holds only counters, so a poisoned lock is still
        // consistent.
        self.limiter.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn say(&self, user: UserId, text: impl Into<String>) -> Reply {
        self.say_with(user, text, None).await
    }

    async fn say_with(
        &self,
        user: UserId,
        text: impl Into<String>,
        keyboard: Option<InlineKeyboardMarkup>,
    ) -> Reply {
        let mut request = self.bot.send_message(ChatId::from(user), text);
        if let Some(keyboard) = keyboard {
            request = request.reply_markup(keyboard);
        }
        request.await?;
        Ok(())
    }

    /// Shows the result of a change that is already committed. A failed edit
    /// must not reach the apology path, since a retry would be wrong: send a
    /// new message instead.
    async fn show_committed(&self, press: Press, text: String) -> Notice {
        let Err(error) = self.edit(press, text.clone(), None).await else {
            return None;
        };
        tracing::warn!(user = %press.user, %error, "editing message with result failed");
        match self.say(press.user, text).await {
            Ok(()) => None,
            Err(_) => Some("Готово, но показать результат не получилось."),
        }
    }

    /// Replaces the pressed message; no keyboard removes the old one.
    async fn edit(
        &self,
        press: Press,
        text: String,
        keyboard: Option<InlineKeyboardMarkup>,
    ) -> Reply {
        let mut request = self
            .bot
            .edit_message_text(ChatId::from(press.user), press.message, text);
        if let Some(keyboard) = keyboard {
            request = request.reply_markup(keyboard);
        }
        match request.await {
            // Paging to the page already shown changes nothing.
            Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(()),
            other => other.map(drop).map_err(Into::into),
        }
    }

    /// The old messages may be gone or too old to edit; that is not worth
    /// failing for.
    async fn drop_keyboards(&self, cancelled: &[CancelledQuiz]) {
        for quiz in cancelled {
            let Some(message) = quiz.message_id else {
                continue;
            };
            let _ = self
                .bot
                .edit_message_reply_markup(ChatId::from(quiz.user_id), TgMessageId(message.0))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: i64, code: &str, is_active: bool) -> CollectionSummary {
        CollectionSummary {
            id: CollectionId(id),
            code: code.parse().unwrap(),
            name: format!("Коллекция {id}"),
            word_count: 3,
            is_active,
        }
    }

    #[test]
    fn collection_list_marks_only_active() {
        let view = View::collections(&[
            summary(1, "K7QM-3XPA", false),
            summary(2, "K7QM-3XPB", true),
        ]);
        let lines: Vec<&str> = view.text.lines().collect();
        assert!(!lines[1].contains("(активная)"));
        assert!(lines[2].contains("(активная)"));
    }
}
