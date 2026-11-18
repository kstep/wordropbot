use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rand::SeedableRng;
use rand::rngs::StdRng;
use teloxide::RequestError;
use teloxide::errors::ApiError;
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, Message, MessageId as TgMessageId,
};

use crate::bot::callback::CallbackAction;
use crate::bot::command::Command;
use crate::bot::limiter::JoinLimiter;
use crate::bot::texts::{
    self, AnswerResult, ImportReport, StatsReport, WORDS_PAGE_SIZE, WordsPage,
};
use crate::bot::{TelegramBot, TelegramSender};
use crate::code::Code;
use crate::config::Config;
use crate::ids::{CollectionId, MessageId, QuizId, UserId, WordId};
use crate::import::ParseReport;
use crate::period::Period;
use crate::service::{DispatchOutcome, QuizService};
use crate::storage::{ImportSummary, Storage};
use crate::time::Timestamp;

const APOLOGY: &str = "Что-то пошло не так, попробуй ещё раз позже.";
const NO_COLLECTION: &str = "Активной коллекции нет. Создай её командой /collection new <название> \
     или подключись по коду: /collection <КОД>.";
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
    message: Option<TgMessageId>,
}

struct WordsView {
    text: String,
    keyboard: Option<InlineKeyboardMarkup>,
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
        let service = QuizService::new(
            storage.clone(),
            TelegramSender::new(bot.clone()),
            config.clone(),
        );
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

    pub async fn on_message(&self, msg: Message) {
        let user = UserId(msg.chat.id.0);
        if let Err(error) = self.handle_message(user, msg.text()).await {
            tracing::error!(%user, %error, "message handler failed");
            // The apology is best effort: the failure may be Telegram itself.
            let _ = self.say(user, APOLOGY).await;
        }
    }

    pub async fn on_callback(&self, query: CallbackQuery) {
        let private = query
            .message
            .as_ref()
            .is_some_and(|m| m.chat().is_private());
        let notice = match i64::try_from(query.from.id.0) {
            Ok(raw_user) if private => {
                // `id()` covers inaccessible messages too, so their edits are not lost.
                let press = Press {
                    user: UserId(raw_user),
                    message: query.message.as_ref().map(|m| m.id()),
                };
                match self.handle_callback(press, query.data.as_deref()).await {
                    Ok(notice) => notice,
                    Err(error) => {
                        tracing::error!(user = %press.user, %error, "callback handler failed");
                        Some(APOLOGY)
                    }
                }
            }
            // Group or inline-mode buttons are not ours; the button still needs an answer.
            _ => None,
        };
        let mut answer = self.bot.answer_callback_query(query.id.clone());
        if let Some(text) = notice {
            answer = answer.text(text);
        }
        if let Err(error) = answer.await {
            tracing::warn!(%error, "answering callback failed");
        }
    }

    async fn handle_message(&self, user: UserId, text: Option<&str>) -> Reply {
        self.storage
            .ensure_user(user, self.config.schedule.default_period, Timestamp::now())
            .await?;
        let Some(text) = text else { return Ok(()) };
        match text.parse::<Command>() {
            Ok(Command::Start | Command::Help | Command::Unknown) => self.help(user).await,
            Ok(Command::CollectionList) => self.collection_list(user).await,
            Ok(Command::CollectionNew(name)) => self.collection_new(user, &name).await,
            Ok(Command::CollectionJoin(raw)) => self.collection_join(user, &raw).await,
            Ok(Command::Words) => self.words(user).await,
            Ok(Command::Remove(term)) => self.remove(user, &term).await,
            Ok(Command::Stats) => self.stats(user).await,
            Ok(Command::Period(raw)) => self.period(user, &raw).await,
            Ok(Command::Pause) => self.pause(user).await,
            Ok(Command::Resume) => self.resume(user).await,
            Ok(Command::Quiz) => self.quiz(user).await,
            Ok(Command::Skip) => self.skip(user).await,
            Err(_) => self.import_text(user, text).await,
        }
    }

    async fn handle_callback(&self, press: Press, data: Option<&str>) -> Reply<Notice> {
        self.storage
            .ensure_user(
                press.user,
                self.config.schedule.default_period,
                Timestamp::now(),
            )
            .await?;
        let Some(Ok(action)) = data.map(str::parse::<CallbackAction>) else {
            return Ok(Some("Эта кнопка больше не работает."));
        };
        match action {
            CallbackAction::Answer { quiz, index } => self.answer(press, quiz, index).await,
            CallbackAction::Activate(id) => self.activate(press, id).await,
            CallbackAction::LeaveAsk(id) => self.leave_ask(press, id).await,
            CallbackAction::LeaveConfirm(id) => self.leave_confirm(press, id).await,
            CallbackAction::RemoveConfirm(id) => self.remove_confirm(press, id).await,
            CallbackAction::WordsPage(page) => self.words_page(press, page).await,
            CallbackAction::Dismiss => self.dismiss(press).await,
        }
    }

    // Commands.

    async fn help(&self, user: UserId) -> Reply {
        self.say(user, texts::HELP).await
    }

    async fn collection_new(&self, user: UserId, name: &str) -> Reply {
        if name.chars().count() > self.config.limits.max_term_chars {
            let max = self.config.limits.max_term_chars;
            return self
                .say(
                    user,
                    format!("Название слишком длинное: не больше {max} символов."),
                )
                .await;
        }
        for _ in 0..CODE_ATTEMPTS {
            let code = Code::generate(&mut rand::rng());
            let created = self
                .storage
                .create_collection(user, name, &code, Timestamp::now())
                .await?;
            if let Some(collection) = created {
                let text = format!(
                    "Коллекция «{}» создана и выбрана активной.\nКод для подключения: {}\n\
                     Теперь отправь слова обычным сообщением.",
                    collection.name, collection.code
                );
                return self.say(user, text).await;
            }
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
        let Ok(code) = raw.parse::<Code>() else {
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
            .await?
        {
            Some(collection) => {
                let text = format!("Ты в коллекции «{}», она теперь активная.", collection.name);
                self.say(user, text).await
            }
            None => {
                self.limiter().record_failure(user, Timestamp::now());
                self.say(user, "Коллекции с таким кодом нет. Проверь код.")
                    .await
            }
        }
    }

    async fn collection_list(&self, user: UserId) -> Reply {
        let collections = self.storage.list_collections(user).await?;
        if collections.is_empty() {
            return self.say(user, NO_COLLECTION).await;
        }
        let mut text = String::from("Твои коллекции:");
        let mut rows = Vec::new();
        for c in &collections {
            let mark = if c.is_active {
                " (активная)"
            } else {
                ""
            };
            text.push_str(&format!(
                "\n{} — {}, слов: {}{mark}",
                c.name, c.code, c.word_count
            ));
            let name: String = c.name.chars().take(BUTTON_NAME_CHARS).collect();
            rows.push(vec![
                Self::button(format!("Выбрать «{name}»"), CallbackAction::Activate(c.id)),
                Self::button("Выйти", CallbackAction::LeaveAsk(c.id)),
            ]);
        }
        self.say_with(user, text, Some(InlineKeyboardMarkup::new(rows)))
            .await
    }

    async fn import_text(&self, user: UserId, text: &str) -> Reply {
        let Some(collection) = self.active_collection(user).await? else {
            return self.say(user, NO_COLLECTION).await;
        };
        let report = ParseReport::parse(text, &self.config.limits);
        let summary = if report.pairs.is_empty() {
            ImportSummary {
                added: 0,
                updated: 0,
            }
        } else {
            self.storage.import_pairs(collection, &report.pairs).await?
        };
        let mut reply = ImportReport {
            summary: &summary,
            bad_lines: &report.bad_lines,
        }
        .to_string();
        if report.pairs.is_empty() {
            reply.push_str("\n\n");
            reply.push_str(IMPORT_FORMAT);
        }
        self.say(user, reply).await
    }

    async fn words(&self, user: UserId) -> Reply {
        match self.words_view(user, 1).await? {
            Some(view) => self.say_with(user, view.text, view.keyboard).await,
            None => self.say(user, NO_COLLECTION).await,
        }
    }

    async fn remove(&self, user: UserId, term: &str) -> Reply {
        let Some(collection) = self.active_collection(user).await? else {
            return self.say(user, NO_COLLECTION).await;
        };
        let Some(word) = self.storage.find_word(collection, term).await? else {
            return self
                .say(user, "Такого слова нет в активной коллекции.")
                .await;
        };
        let text = format!(
            "Удалить «{}»? Слово исчезнет у всех участников коллекции вместе с историей ответов.",
            word.term
        );
        let keyboard = InlineKeyboardMarkup::new([[
            Self::button("Удалить", CallbackAction::RemoveConfirm(word.id)),
            Self::button("Отмена", CallbackAction::Dismiss),
        ]]);
        self.say_with(user, text, Some(keyboard)).await
    }

    async fn stats(&self, user: UserId) -> Reply {
        let Some(collection) = self.active_collection(user).await? else {
            return self.say(user, NO_COLLECTION).await;
        };
        let stats = self
            .storage
            .collection_stats(user, collection, Timestamp::now(), &self.config.selection)
            .await?;
        self.say(user, StatsReport(&stats).to_string()).await
    }

    async fn period(&self, user: UserId, raw: &str) -> Reply {
        let Ok(period) = raw.parse::<Period>() else {
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
        let mut text = format!("Период квизов: {period}.");
        if quiz_open {
            text.push('\n');
            text.push_str(OPEN_QUIZ_NOTE);
        }
        self.say(user, text).await
    }

    async fn pause(&self, user: UserId) -> Reply {
        self.storage
            .set_paused(user, true, Timestamp::now())
            .await?;
        self.say(user, "Квизы приостановлены. Возобновить: /resume.")
            .await
    }

    async fn resume(&self, user: UserId) -> Reply {
        let quiz_open = self
            .storage
            .set_paused(user, false, Timestamp::now())
            .await?;
        let mut text = String::from("Квизы возобновлены.");
        if quiz_open {
            text.push('\n');
            text.push_str(OPEN_QUIZ_NOTE);
        }
        self.say(user, text).await
    }

    async fn quiz(&self, user: UserId) -> Reply {
        if let Some(open) = self.storage.open_quiz(user).await? {
            let sent = self
                .bot
                .send_message(ChatId(user.0), open.term)
                .reply_markup(TelegramSender::keyboard(open.id, &open.options))
                .await?;
            self.storage
                .set_quiz_message(open.id, MessageId(sent.id.0))
                .await?;
            self.drop_keyboard(user, open.message_id).await;
            return Ok(());
        }
        let mut rng = StdRng::from_os_rng();
        match self
            .service
            .dispatch(user, Timestamp::now(), &mut rng)
            .await?
        {
            DispatchOutcome::Sent | DispatchOutcome::Blocked => Ok(()),
            DispatchOutcome::AlreadyOpen => self.say(user, "Квиз уже открыт, повтори /quiz.").await,
            DispatchOutcome::NoEligibleWords => {
                self.say(
                    user,
                    "Пока недостаточно слов для квиза: добавь ещё слов в активную коллекцию.",
                )
                .await
            }
            DispatchOutcome::Failed => self.say(user, APOLOGY).await,
        }
    }

    async fn skip(&self, user: UserId) -> Reply {
        match self.storage.skip_quiz(user, Timestamp::now()).await? {
            Some(cancelled) => {
                self.drop_keyboard(user, cancelled.message_id).await;
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
            Some(result) => {
                // The answer is already stored, so a failed edit must not ask for a retry.
                let text = result.to_string();
                if let Err(error) = self.edit(press, text.clone(), None).await {
                    tracing::warn!(user = %press.user, %error, "showing quiz result failed");
                    if self.say(press.user, text).await.is_err() {
                        return Ok(Some("Ответ засчитан, но показать результат не получилось."));
                    }
                }
                Ok(None)
            }
            None => Ok(Some("Этот квиз уже закрыт.")),
        }
    }

    async fn activate(&self, press: Press, id: CollectionId) -> Reply<Notice> {
        if !self.storage.set_active_collection(press.user, id).await? {
            return Ok(Some("Ты не участник этой коллекции."));
        }
        Ok(Some("Коллекция выбрана."))
    }

    async fn leave_ask(&self, press: Press, id: CollectionId) -> Reply<Notice> {
        let collections = self.storage.list_collections(press.user).await?;
        let Some(collection) = collections.iter().find(|c| c.id == id) else {
            return Ok(Some("Ты не участник этой коллекции."));
        };
        let text = format!(
            "Выйти из коллекции «{}»? История ответов сохранится, открытый квиз из неё отменится. \
             Если ты последний участник, коллекция удалится со всеми словами.",
            collection.name
        );
        let keyboard = InlineKeyboardMarkup::new([[
            Self::button("Выйти", CallbackAction::LeaveConfirm(id)),
            Self::button("Отмена", CallbackAction::Dismiss),
        ]]);
        self.bot
            .send_message(ChatId(press.user.0), text)
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
        for quiz in cancelled {
            self.drop_keyboard(quiz.user_id, quiz.message_id).await;
        }
        self.edit(press, "Ты вышел из коллекции.".into(), None)
            .await?;
        Ok(None)
    }

    async fn remove_confirm(&self, press: Press, id: WordId) -> Reply<Notice> {
        let Some(cancelled) = self
            .storage
            .remove_word(press.user, id, Timestamp::now())
            .await?
        else {
            return Ok(Some("Слово уже удалено или недоступно."));
        };
        for quiz in cancelled {
            self.drop_keyboard(quiz.user_id, quiz.message_id).await;
        }
        self.edit(press, "Слово удалено.".into(), None).await?;
        Ok(None)
    }

    async fn words_page(&self, press: Press, page: i64) -> Reply<Notice> {
        let Some(collection) = self.active_collection(press.user).await? else {
            return Ok(Some("Активной коллекции нет."));
        };
        if !self.storage.is_member(press.user, collection).await? {
            return Ok(Some("Ты не участник этой коллекции."));
        }
        match self.words_view(press.user, page).await? {
            Some(view) => self.edit(press, view.text, view.keyboard).await?,
            None => return Ok(Some("Активной коллекции нет.")),
        }
        Ok(None)
    }

    async fn dismiss(&self, press: Press) -> Reply<Notice> {
        if let Some(message) = press.message {
            // Deleting is cosmetic: a failure only leaves the stale buttons in place.
            let _ = self.bot.delete_message(ChatId(press.user.0), message).await;
        }
        Ok(Some("Отменено."))
    }

    // Helpers.

    /// `None` when the user has no active collection. The page is clamped to the valid range.
    async fn words_view(&self, user: UserId, page: i64) -> Reply<Option<WordsView>> {
        let Some(collection) = self.active_collection(user).await? else {
            return Ok(None);
        };
        let total = self.storage.count_words(collection).await?;
        if total == 0 {
            return Ok(Some(WordsView {
                text: "В коллекции пока нет слов.".into(),
                keyboard: None,
            }));
        }
        let total_pages = (total + WORDS_PAGE_SIZE - 1) / WORDS_PAGE_SIZE;
        let page = page.clamp(1, total_pages);
        let words = self
            .storage
            .list_words(collection, (page - 1) * WORDS_PAGE_SIZE, WORDS_PAGE_SIZE)
            .await?;
        let mut row = Vec::new();
        if page > 1 {
            row.push(Self::button("◀", CallbackAction::WordsPage(page - 1)));
        }
        if page < total_pages {
            row.push(Self::button("▶", CallbackAction::WordsPage(page + 1)));
        }
        Ok(Some(WordsView {
            text: WordsPage {
                words: &words,
                page,
                total_pages,
            }
            .to_string(),
            keyboard: (!row.is_empty()).then(|| InlineKeyboardMarkup::new([row])),
        }))
    }

    async fn active_collection(&self, user: UserId) -> Reply<Option<CollectionId>> {
        let user = self.storage.get_user(user).await?;
        Ok(user.and_then(|u| u.active_collection_id))
    }

    fn limiter(&self) -> MutexGuard<'_, JoinLimiter> {
        // The limiter holds only counters, so a poisoned lock is still consistent.
        self.limiter.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn button(label: impl Into<String>, action: CallbackAction) -> InlineKeyboardButton {
        InlineKeyboardButton::callback(label, action.to_string())
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
        let request = self.bot.send_message(ChatId(user.0), text);
        match keyboard {
            Some(keyboard) => request.reply_markup(keyboard).await?,
            None => request.await?,
        };
        Ok(())
    }

    /// Replaces the pressed message (or sends a new one if there is none); no keyboard removes the old one.
    async fn edit(
        &self,
        press: Press,
        text: String,
        keyboard: Option<InlineKeyboardMarkup>,
    ) -> Reply {
        let Some(message) = press.message else {
            return self.say_with(press.user, text, keyboard).await;
        };
        let request = self
            .bot
            .edit_message_text(ChatId(press.user.0), message, text);
        let result = match keyboard {
            Some(keyboard) => request.reply_markup(keyboard).await,
            None => request.await,
        };
        match result {
            // Paging to the page already shown changes nothing.
            Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(()),
            other => other.map(drop).map_err(Into::into),
        }
    }

    /// The old message may be gone or too old to edit; that is not worth failing for.
    async fn drop_keyboard(&self, user: UserId, message: Option<MessageId>) {
        let Some(message) = message else { return };
        let _ = self
            .bot
            .edit_message_reply_markup(ChatId(user.0), TgMessageId(message.0))
            .await;
    }
}
