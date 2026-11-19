use std::{
    fmt::Write as _,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use chrono::Utc;
use rand::rngs::StdRng;
use teloxide::{
    RequestError,
    errors::ApiError,
    prelude::*,
    requests::JsonRequest,
    types::{
        CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, Message, MessageEntity,
        MessageId as TgMessageId,
    },
};
use tokio::sync::OnceCell;

use crate::{
    bot::{
        TelegramBot, TelegramSender,
        callback::CallbackAction,
        command::Command,
        gate::UserGate,
        limiter::JoinLimiter,
        page::Page,
        rich::{EditRichMessage, SendRichMessage},
        texts::{
            self, PAUSED_NOTE, WORDS_PAGE_SIZE, missing_collection_hint, write_word_shortage_hint,
        },
    },
    code::Code,
    config::Config,
    ids::{CollectionId, QuizId, UserId, WordId},
    import::ParseReport,
    period::Period,
    service::{DispatchOutcome, QuizService},
    storage::{
        CancelledQuiz, CollectionError, CollectionName, CollectionNameError, CollectionSummary,
        Storage, User,
    },
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
/// Ten list lines with names this long stay within Telegram's message limit.
const LIST_NAME_CHARS: usize = 200;
/// Keeps one page within Telegram's limits on text and buttons.
const COLLECTIONS_PAGE_SIZE: u32 = 10;
const NOT_MEMBER_NOTICE: &str = "Ты не участник этой коллекции.";
const BUSY_NOTICE: &str = "Слишком много запросов, подожди немного.";
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

/// How the text of a `View` is formatted.
enum Markup {
    /// Plain text with these entities.
    Entities(Vec<MessageEntity>),
    /// Rich message HTML.
    RichHtml,
}

struct View {
    text: String,
    keyboard: Option<InlineKeyboardMarkup>,
    markup: Markup,
}

impl View {
    fn plain(text: impl Into<String>) -> View {
        View {
            text: text.into(),
            keyboard: None,
            markup: Markup::Entities(Vec::new()),
        }
    }

    /// Appends `code` shown in monospace.
    fn push_code(&mut self, code: &str) {
        let Markup::Entities(entities) = &mut self.markup else {
            unreachable!("codes are marked in plain views only");
        };
        let offset = self.text.encode_utf16().count();
        entities.push(MessageEntity::code(offset, code.encode_utf16().count()));
        self.text.push_str(code);
    }

    /// One page of the collection list; the page is clamped to the valid range.
    /// With the bot's username each collection gets its invite link.
    fn collections(collections: &[CollectionSummary], page: Page, bot: Option<&str>) -> View {
        let last = Page::last_for(collections.len(), COLLECTIONS_PAGE_SIZE);
        let page = page.min(last);
        let start = (page.offset(COLLECTIONS_PAGE_SIZE) as usize).min(collections.len());
        let end = (start + COLLECTIONS_PAGE_SIZE as usize).min(collections.len());
        let mut view = View::plain(if last > Page::FIRST {
            format!(
                "Твои коллекции, страница {} из {}:",
                page.number(),
                last.number()
            )
        } else {
            String::from("Твои коллекции:")
        });
        let mut rows = Vec::new();
        for collection in &collections[start..end] {
            let mark = if collection.is_active {
                " (активная)"
            } else {
                ""
            };
            // Writing to a String cannot fail.
            let _ = write!(
                view.text,
                "\n{} — ",
                first_chars(&collection.name, LIST_NAME_CHARS)
            );
            view.push_code(collection.code.as_str());
            let _ = write!(view.text, ", слов: {}{mark}", collection.word_count);
            if let Some(bot) = bot {
                let _ = write!(view.text, "\n{}", invite_link(bot, &collection.code));
            }
            let name = first_chars(&collection.name, BUTTON_NAME_CHARS);
            rows.push(vec![
                CallbackAction::Activate(collection.id).button(format!("Выбрать «{name}»")),
                CallbackAction::LeaveAsk(collection.id).button("Выйти"),
            ]);
        }
        let navigation = View::navigation(page, last, CallbackAction::CollectionsPage);
        if !navigation.is_empty() {
            rows.push(navigation);
        }
        view.keyboard = Some(InlineKeyboardMarkup::new(rows));
        view
    }

    /// The page of the list that shows this collection.
    fn collections_page_of(collections: &[CollectionSummary], id: CollectionId) -> Page {
        let position = collections
            .iter()
            .position(|collection| collection.id == id)
            .unwrap_or(0);
        Page::of_item(position, COLLECTIONS_PAGE_SIZE)
    }

    /// Buttons to the neighbouring pages that exist.
    fn navigation(
        page: Page,
        last: Page,
        action: fn(Page) -> CallbackAction,
    ) -> Vec<InlineKeyboardButton> {
        let previous = page.previous().map(|page| action(page).button("◀"));
        let next = page.next_within(last).map(|page| action(page).button("▶"));
        previous.into_iter().chain(next).collect()
    }

    /// One page of the collection's words; the page is clamped to the valid
    /// range.
    async fn words(
        storage: &Storage,
        collection: CollectionId,
        page: Page,
    ) -> Result<View, sqlx::Error> {
        let total = storage.count_words(collection).await?;
        if total == 0 {
            return Ok(View::plain("В коллекции пока нет слов."));
        }
        let last = Page::last_for(total as usize, WORDS_PAGE_SIZE);
        let page = page.min(last);
        let words = storage
            .list_words(collection, page.offset(WORDS_PAGE_SIZE), WORDS_PAGE_SIZE)
            .await?;
        let row = View::navigation(page, last, CallbackAction::WordsPage);
        Ok(View {
            text: texts::words_html(&words, page, last),
            keyboard: (!row.is_empty()).then(|| InlineKeyboardMarkup::new([row])),
            markup: Markup::RichHtml,
        })
    }
}

/// Link that opens the bot and joins the collection with `code`.
fn invite_link(bot: &str, code: &Code) -> String {
    format!("https://t.me/{bot}?start={code}")
}

/// The first `max_chars` characters of `text`.
fn first_chars(text: &str, max_chars: usize) -> &str {
    let end = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(index, _)| index);
    &text[..end]
}

pub struct Handlers {
    bot: TelegramBot,
    storage: Storage,
    service: QuizService<TelegramSender>,
    config: Arc<Config>,
    limiter: Mutex<JoinLimiter>,
    gate: Arc<UserGate>,
    username: OnceCell<String>,
}

impl Handlers {
    pub fn new(
        bot: TelegramBot,
        storage: Storage,
        service: QuizService<TelegramSender>,
        config: Arc<Config>,
    ) -> Handlers {
        let limiter = Mutex::new(JoinLimiter::new(config.limits.join_attempts_per_minute));
        let gate = UserGate::new(config.limits.max_pending_updates_per_user);
        Handlers {
            bot,
            storage,
            service,
            config,
            limiter,
            gate,
            username: OnceCell::new(),
        }
    }

    /// The bot's username for invite links, asked from Telegram once. Without
    /// it messages go out with no links.
    async fn bot_username(&self) -> Option<&str> {
        let username = self
            .username
            .get_or_try_init(|| async {
                Ok::<_, RequestError>(self.bot.get_me().await?.username().to_owned())
            })
            .await;
        match username {
            Ok(username) => Some(username),
            Err(error) => {
                tracing::warn!(%error, "getting bot username failed");
                None
            }
        }
    }

    // Entry points: failures are logged and never reach the dispatcher.

    pub async fn on_message(&self, message: Message) {
        let user = UserId(message.chat.id.0);
        let Some(mut admission) = self.gate.admit(user) else {
            tracing::warn!(%user, "too many pending updates, dropping message");
            return;
        };
        admission.wait_turn().await;
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
            Some(press) => match self.gate.admit(press.user) {
                Some(mut admission) => {
                    admission.wait_turn().await;
                    self.press_notice(press, query.data.as_deref()).await
                }
                None => {
                    tracing::warn!(user = %press.user, "too many pending updates, dropping button press");
                    Some(BUSY_NOTICE)
                }
            },
            // Group or inline-mode buttons are not ours; the button still needs
            // an answer.
            None => None,
        };
        let mut answer = self.bot.answer_callback_query(query.id);
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
            Ok(Command::Start(code)) => {
                if let Some(code) = code {
                    self.collection_join(user.id, &code).await?;
                }
                self.help(user, user.paused).await
            }
            Ok(Command::Help | Command::Unknown) => self.help(user, false).await,
            Ok(Command::CollectionList) => self.collection_list(user.id).await,
            Ok(Command::CollectionNew(name)) => self.collection_new(user.id, &name).await,
            Ok(Command::CollectionJoin(raw)) => self.collection_join(user.id, &raw).await,
            Ok(Command::Words) => self.words(user).await,
            Ok(Command::Remove(term)) => self.remove(user, &term).await,
            Ok(Command::Stats) => self.stats(user).await,
            Ok(Command::Period(None)) => self.show_period(user).await,
            Ok(Command::Period(Some(raw))) => self.period(user.id, &raw).await,
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
            CallbackAction::CollectionsPage(page) => self.collections_page(press, page).await,
            CallbackAction::Dismiss => self.dismiss(press).await,
        }
    }

    async fn help(&self, user: User, paused: bool) -> Reply {
        let text = texts::help(self.config.quiz.options, user.period, paused);
        self.say(user.id, text).await
    }

    async fn show_period(&self, user: User) -> Reply {
        let mut text = format!(
            "Квизы приходят раз в {}. Изменить: /period 30m, /period 2h, /period 1d.",
            user.period.text()
        );
        if user.paused {
            text.push('\n');
            text.push_str(PAUSED_NOTE);
        }
        self.say(user.id, text).await
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
                .create_collection(user, name.as_str(), &code, Utc::now())
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
            let mut view = View::plain(format!(
                "Коллекция «{}» создана и выбрана активной.\nКод для подключения: ",
                collection.name
            ));
            view.push_code(collection.code.as_str());
            if let Some(bot) = self.bot_username().await {
                let link = invite_link(bot, &collection.code);
                let _ = write!(view.text, "\nСсылка для друзей: {link}");
            }
            view.text.push_str("\n\n");
            view.text.push_str(IMPORT_FORMAT);
            // The collection is saved: the apology path would invite a retry
            // that creates a second one.
            if let Err(error) = self.show(user, view).await {
                tracing::warn!(%user, %error, "reporting created collection failed");
            }
            return Ok(());
        }
        Err(HandlerError::NoFreeCode)
    }

    async fn collection_join(&self, user: UserId, raw: &str) -> Reply {
        let now = Utc::now();
        if !self.limiter().allow(user, now) {
            return self
                .say(
                    user,
                    "Слишком много неверных попыток. Подожди минуту и попробуй снова.",
                )
                .await;
        }
        let Ok(code): Result<Code, _> = raw.parse() else {
            self.limiter().record_failure(user, now);
            let mut view = View::plain("Код выглядит неверно: нужно 8 символов, например ");
            view.push_code("K7QM-3XPA");
            view.text.push('.');
            return self.show(user, view).await;
        };
        match self.storage.join_collection(user, &code, now).await {
            Ok(Some(collection)) => {
                let text = format!("Ты в коллекции «{}», она теперь активная.", collection.name);
                self.say(user, text).await
            }
            Ok(None) => {
                self.limiter().record_failure(user, now);
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
        let view = View::collections(&collections, Page::FIRST, self.bot_username().await);
        self.show(user, view).await
    }

    async fn import_text(&self, user: User, text: &str) -> Reply {
        let Some(collection) = self.require_collection(user).await? else {
            return Ok(());
        };
        let report = ParseReport::parse(text, &self.config.limits);
        let summary = self.storage.import_pairs(collection, &report.pairs).await?;
        let mut reply = summary.report(&report.bad_lines, &self.config.limits);
        if report.pairs.is_empty() {
            reply.push_str("\n\n");
            reply.push_str(IMPORT_FORMAT);
        } else {
            // The import is saved, so a failed check only costs the hint.
            match self.service.has_eligible_words(user.id, Utc::now()).await {
                Ok(true) => {}
                Ok(false) => {
                    reply.push_str("\n\n");
                    // Writing to a String cannot fail.
                    let _ = write_word_shortage_hint(&mut reply, self.config.quiz.options);
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
        let view = View::words(&self.storage, collection, Page::FIRST).await?;
        self.show(user.id, view).await
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
        let view = View {
            keyboard: Some(keyboard),
            ..View::plain(text)
        };
        self.show(user.id, view).await
    }

    async fn stats(&self, user: User) -> Reply {
        let Some(collection) = self.require_collection(user).await? else {
            return Ok(());
        };
        let stats = self
            .storage
            .collection_stats(user.id, collection, Utc::now(), &self.config.selection)
            .await?;
        self.say(user.id, stats.report()).await
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
                .say(
                    user,
                    format!("Чаще, чем раз в {}, квизы не приходят.", min.text()),
                )
                .await;
        }
        let quiz_open = self.storage.set_period(user, period, Utc::now()).await?;
        let text = Self::with_open_quiz_note(
            format!("Теперь квизы приходят раз в {}.", period.text()),
            quiz_open,
        );
        self.say(user, text).await
    }

    async fn pause(&self, user: UserId) -> Reply {
        self.storage.set_paused(user, true, Utc::now()).await?;
        self.say(user, PAUSED_NOTE).await
    }

    async fn resume(&self, user: UserId) -> Reply {
        let quiz_open = self.storage.set_paused(user, false, Utc::now()).await?;
        let text = Self::with_open_quiz_note("Квизы возобновлены.".into(), quiz_open);
        self.say(user, text).await
    }

    async fn quiz(&self, user: UserId) -> Reply {
        let mut random_generator: StdRng = rand::make_rng();
        match self
            .service
            .request_quiz(user, Utc::now(), &mut random_generator)
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
        match self.storage.skip_quiz(user, Utc::now()).await? {
            Some(cancelled) => {
                self.drop_keyboards(&[cancelled]).await;
                self.say(user, "Квиз пропущен, ошибкой это не считается.")
                    .await
            }
            None => self.say(user, "Открытого квиза нет.").await,
        }
    }

    async fn answer(&self, press: Press, quiz: QuizId, index: usize) -> Reply<Notice> {
        let outcome = self
            .storage
            .answer_quiz(quiz, press.user, index, Utc::now())
            .await?;
        match outcome.text() {
            Some(text) => Ok(self.show_committed(press, &text).await),
            None => Ok(Some("Этот квиз уже закрыт.")),
        }
    }

    async fn activate(&self, press: Press, id: CollectionId) -> Reply<Notice> {
        if !self.storage.set_active_collection(press.user, id).await? {
            return Ok(Some(NOT_MEMBER_NOTICE));
        }
        // Refresh the "(активная)" mark. The choice is saved, so a failed
        // refresh is not an error.
        if let Err(error) = self.refresh_collections(press, id).await {
            tracing::warn!(user = %press.user, %error, "refreshing collection list failed");
        }
        Ok(Some("Коллекция выбрана."))
    }

    async fn refresh_collections(&self, press: Press, id: CollectionId) -> Reply {
        let collections = self.storage.list_collections(press.user).await?;
        let page = View::collections_page_of(&collections, id);
        let view = View::collections(&collections, page, self.bot_username().await);
        self.edit(press, view).await
    }

    async fn collections_page(&self, press: Press, page: Page) -> Reply<Notice> {
        let collections = self.storage.list_collections(press.user).await?;
        if collections.is_empty() {
            return Ok(Some("У тебя нет коллекций."));
        }
        let view = View::collections(&collections, page, self.bot_username().await);
        self.edit(press, view).await?;
        Ok(None)
    }

    async fn leave_ask(&self, press: Press, id: CollectionId) -> Reply<Notice> {
        let collections = self.storage.list_collections(press.user).await?;
        let Some(collection) = collections.iter().find(|listed| listed.id == id) else {
            return Ok(Some(NOT_MEMBER_NOTICE));
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
            .leave_collection(press.user, id, Utc::now())
            .await?
        else {
            return Ok(Some(NOT_MEMBER_NOTICE));
        };
        self.drop_keyboards(&cancelled).await;
        Ok(self.show_committed(press, "Ты вышел из коллекции.").await)
    }

    async fn remove_confirm(&self, press: Press, id: WordId) -> Reply<Notice> {
        let Some(cancelled) = self.storage.remove_word(press.user, id, Utc::now()).await? else {
            return Ok(Some("Слово уже удалено или недоступно."));
        };
        self.drop_keyboards(&cancelled).await;
        Ok(self.show_committed(press, "Слово удалено.").await)
    }

    async fn words_page(&self, press: Press, user: User, page: Page) -> Reply<Notice> {
        // The active collection always belongs to the user: leaving clears it
        // in the same transaction as the membership.
        let Some(collection) = user.active_collection_id else {
            return Ok(Some("Активной коллекции нет."));
        };
        let view = View::words(&self.storage, collection, page).await?;
        self.edit(press, view).await?;
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

    async fn ensure_user(&self, id: UserId) -> Result<User, sqlx::Error> {
        self.storage
            .ensure_user(id, self.config.schedule.default_period, Utc::now())
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
        let has_collections = self.storage.has_collections(user).await?;
        self.say(user, missing_collection_hint(has_collections))
            .await
    }

    fn collection_limit_text(max: u32) -> String {
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
        self.show(user, View::plain(text)).await
    }

    async fn show(&self, user: UserId, view: View) -> Reply {
        let chat = ChatId::from(user);
        match view.markup {
            Markup::Entities(entities) => {
                let mut request = self.bot.send_message(chat, view.text);
                if let Some(keyboard) = view.keyboard {
                    request = request.reply_markup(keyboard);
                }
                if !entities.is_empty() {
                    request = request.entities(entities);
                }
                request.await?;
            }
            Markup::RichHtml => {
                let payload = SendRichMessage::new(chat, view.text, view.keyboard);
                JsonRequest::new(self.bot.inner().clone(), payload).await?;
            }
        }
        Ok(())
    }

    /// Shows the result of a change that is already committed. A failed edit
    /// must not reach the apology path, since a retry would be wrong: send a
    /// new message instead.
    async fn show_committed(&self, press: Press, text: &str) -> Notice {
        let Err(error) = self.edit(press, View::plain(text)).await else {
            return None;
        };
        tracing::warn!(user = %press.user, %error, "editing message with result failed");
        match self.say(press.user, text).await {
            Ok(()) => None,
            Err(_) => Some("Готово, но показать результат не получилось."),
        }
    }

    /// Replaces the pressed message; no keyboard removes the old one.
    async fn edit(&self, press: Press, view: View) -> Reply {
        let chat = ChatId::from(press.user);
        let result = match view.markup {
            Markup::Entities(entities) => {
                let mut request = self.bot.edit_message_text(chat, press.message, view.text);
                if let Some(keyboard) = view.keyboard {
                    request = request.reply_markup(keyboard);
                }
                if !entities.is_empty() {
                    request = request.entities(entities);
                }
                request.await.map(drop)
            }
            Markup::RichHtml => {
                let payload = EditRichMessage::new(chat, press.message, view.text, view.keyboard);
                JsonRequest::new(self.bot.inner().clone(), payload)
                    .await
                    .map(drop)
            }
        };
        match result {
            // Paging to the page already shown changes nothing.
            Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(()),
            other => other.map_err(Into::into),
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
    use teloxide::types::InlineKeyboardButtonKind;

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

    fn callback_data(view: &View) -> Vec<String> {
        view.keyboard
            .iter()
            .flat_map(|keyboard| keyboard.inline_keyboard.iter().flatten())
            .filter_map(|button| match &button.kind {
                InlineKeyboardButtonKind::CallbackData(data) => Some(data.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn collection_list_marks_only_active() {
        let view = View::collections(
            &[
                summary(1, "K7QM-3XPA", false),
                summary(2, "K7QM-3XPB", true),
            ],
            Page::FIRST,
            None,
        );
        let lines: Vec<&str> = view.text.lines().collect();
        assert!(!lines[1].contains("(активная)"));
        assert!(lines[2].contains("(активная)"));
    }

    #[test]
    fn long_collection_list_stays_within_limits_and_reachable() {
        let collections: Vec<CollectionSummary> = (0..60)
            .map(|id| CollectionSummary {
                name: "щ".repeat(200),
                ..summary(id, "K7QM-3XPA", false)
            })
            .collect();
        let mut reachable = Vec::new();
        for page in 0..6 {
            // Telegram usernames have at most 32 characters.
            let view = View::collections(&collections, Page::new(page), Some(&"b".repeat(32)));
            assert!(view.text.chars().count() <= 4096, "page {page}");
            let data = callback_data(&view);
            assert!(data.len() <= 100, "page {page}");
            reachable.extend(data);
        }
        for id in 0..60 {
            assert!(reachable.contains(&format!("c:{id}")), "activate {id}");
            assert!(reachable.contains(&format!("l:{id}")), "leave {id}");
        }
        let last = View::collections(&collections, Page::new(99), None);
        assert!(callback_data(&last).contains(&"c:59".to_owned()));
    }

    #[test]
    fn collection_codes_are_monospace() {
        let collections: Vec<CollectionSummary> = (0..12)
            .map(|id| {
                summary(
                    id,
                    if id % 2 == 0 {
                        "K7QM-3XPA"
                    } else {
                        "AAAA-AAAA"
                    },
                    false,
                )
            })
            .collect();
        let view = View::collections(&collections, Page::new(1), Some("wordropbot"));
        assert!(
            view.text
                .contains("\nhttps://t.me/wordropbot?start=K7QM-3XPA"),
            "{}",
            view.text
        );
        let Markup::Entities(entities) = &view.markup else {
            panic!("collection list must be plain text");
        };
        assert_eq!(entities.len(), 2);
        for entity in entities {
            let code: String = view
                .text
                .encode_utf16()
                .skip(entity.offset)
                .take(entity.length)
                .map(|unit| char::from_u32(u32::from(unit)).unwrap())
                .collect();
            assert!(
                ["K7QM-3XPA", "AAAA-AAAA"].contains(&code.as_str()),
                "{code}"
            );
        }
    }

    #[test]
    fn activated_collection_keeps_its_page() {
        let collections: Vec<CollectionSummary> =
            (0..25).map(|id| summary(id, "K7QM-3XPA", false)).collect();
        assert_eq!(
            View::collections_page_of(&collections, CollectionId(0)),
            Page::FIRST
        );
        assert_eq!(
            View::collections_page_of(&collections, CollectionId(9)),
            Page::FIRST
        );
        assert_eq!(
            View::collections_page_of(&collections, CollectionId(10)),
            Page::new(1)
        );
        assert_eq!(
            View::collections_page_of(&collections, CollectionId(24)),
            Page::new(2)
        );
    }
}
