use std::fmt::{self, Write as _};

use crate::{
    bot::page::Page,
    config::{LimitsConfig, MAX_MESSAGE_CHARS},
    storage::{AnswerOutcome, CollectionStats, ImportSummary, WordView},
};

pub const WORDS_PAGE_SIZE: i64 = 20;

const MAX_BAD_LINES: usize = 20;
/// Leaves room for the page header, so all lines of a full page fit one message
/// and no word is cut off the end.
const MAX_WORD_LINE_CHARS: usize = 200;

const HELP: &str = "\
Я помогаю учить слова: присылаю квизы с вариантами перевода.

Чтобы добавить слова, отправь обычное сообщение: по паре «слово перевод» в строке. \
Если у термина или перевода несколько слов, раздели их через « - ». \
Несколько переводов одного слова пиши отдельными строками.

Команды:
/collection - твои коллекции
/collection new <название> - создать коллекцию
/collection <КОД> - подключиться к коллекции по коду
/words - слова активной коллекции
/remove <слово> - удалить слово у всех участников
/stats - статистика активной коллекции
/period <длительность> - как часто присылать квизы: 30m, 2h, 1d
/pause - приостановить квизы
/resume - возобновить квизы
/quiz - прислать открытый квиз заново или получить новый сразу
/skip - отменить открытый квиз без ошибки
/help - эта справка";

pub const PAUSED_NOTE: &str = "Квизы приостановлены. Возобновить: /resume.";

/// A number of words with the matching Russian noun form: 1 слово, 2 слова,
/// 5 слов, 11 слов, 21 слово.
struct WordCount(usize);

impl fmt::Display for WordCount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let noun = match (self.0 % 100, self.0 % 10) {
            (11..=14, _) => "слов",
            (_, 1) => "слово",
            (_, 2..=4) => "слова",
            _ => "слов",
        };
        write!(formatter, "{} {noun}", self.0)
    }
}

/// The help message, which also names how many words a quiz needs.
pub struct Help {
    /// `quiz.options`: a quiz needs that many words with different
    /// translations.
    pub quiz_options: usize,
    pub paused: bool,
}

impl fmt::Display for Help {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.paused {
            writeln!(formatter, "{PAUSED_NOTE}\n")?;
        }
        write!(
            formatter,
            "{HELP}\n\nКвизы начнутся, когда в коллекции будет минимум {} с разными переводами.",
            WordCount(self.quiz_options)
        )
    }
}

/// Added to an import reply when no word can be asked yet.
pub struct NotEnoughWords {
    pub quiz_options: usize,
}

impl fmt::Display for NotEnoughWords {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Квизы пока не начнутся: добавь слов, чтобы в коллекции было минимум {} с разными переводами.",
            WordCount(self.quiz_options)
        )
    }
}

/// Reply to a command that needs an active collection when there is none.
pub struct NoActiveCollection {
    pub has_collections: bool,
}

impl fmt::Display for NoActiveCollection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.has_collections {
            formatter.write_str("Активная коллекция не выбрана. Выбери её через /collection.")
        } else {
            formatter.write_str(
                "Активной коллекции нет. Создай её командой /collection new <название> \
                 или подключись по коду: /collection <КОД>.",
            )
        }
    }
}

/// Writer that passes at most `max_chars` characters to `inner`: longer text
/// keeps its first `max_chars - 1` characters and ends with an ellipsis. Text
/// is written through as it comes, without buffering it.
pub struct Truncating<W> {
    inner: W,
    max_chars: usize,
    written: usize,
    /// The character in the last slot, written only if nothing follows it.
    last: Option<char>,
    is_cut: bool,
}

impl<W: fmt::Write> Truncating<W> {
    pub fn new(inner: W, max_chars: usize) -> Truncating<W> {
        debug_assert!(max_chars > 0);
        Truncating {
            inner,
            max_chars,
            written: 0,
            last: None,
            is_cut: false,
        }
    }

    /// Telegram's message limit.
    pub fn message(inner: W) -> Truncating<W> {
        Truncating::new(inner, MAX_MESSAGE_CHARS)
    }

    pub fn finish(mut self) -> fmt::Result {
        match self.last {
            Some(character) => self.inner.write_char(character),
            None => Ok(()),
        }
    }

    fn cut(&mut self) -> fmt::Result {
        self.is_cut = true;
        self.inner.write_char('…')
    }
}

impl<W: fmt::Write> fmt::Write for Truncating<W> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.is_cut || text.is_empty() {
            return Ok(());
        }
        if self.last.is_some() {
            return self.cut();
        }
        let room = self.max_chars - 1 - self.written;
        for (count, (index, character)) in text.char_indices().enumerate() {
            if count == room {
                self.inner.write_str(&text[..index])?;
                self.written += count;
                if index + character.len_utf8() == text.len() {
                    self.last = Some(character);
                    return Ok(());
                }
                return self.cut();
            }
        }
        self.written += text.chars().count();
        self.inner.write_str(text)
    }
}

/// Text cut with an ellipsis if it exceeds Telegram's message limit.
pub struct Truncated<'a>(pub &'a str);

impl fmt::Display for Truncated<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut output = Truncating::message(formatter);
        output.write_str(self.0)?;
        output.finish()
    }
}

/// Translations listed with commas.
struct Joined<'a>(&'a [String]);

impl fmt::Display for Joined<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, item) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            formatter.write_str(item)?;
        }
        Ok(())
    }
}

/// One word of the `/words` listing, cut to its share of the page.
struct WordLine<'a>(&'a WordView);

impl fmt::Display for WordLine<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let word = self.0;
        let mut output = Truncating::new(formatter, MAX_WORD_LINE_CHARS);
        write!(output, "{} — {}", word.term, Joined(&word.translations))?;
        output.finish()
    }
}

/// One page of the `/words` listing.
pub struct WordsPage<'a> {
    pub words: &'a [WordView],
    pub page: Page,
    pub total_pages: i64,
}

impl fmt::Display for WordsPage<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut output = Truncating::message(formatter);
        write!(
            output,
            "Слова, страница {} из {}:",
            self.page, self.total_pages
        )?;
        for word in self.words {
            write!(output, "\n{}", WordLine(word))?;
        }
        output.finish()
    }
}

/// Result of a text import.
pub struct ImportReport<'a> {
    pub summary: &'a ImportSummary,
    pub bad_lines: &'a [usize],
    pub limits: &'a LimitsConfig,
}

impl fmt::Display for ImportReport<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Добавлено слов: {}, обновлено: {}.",
            self.summary.added, self.summary.updated
        )?;
        if !self.bad_lines.is_empty() {
            formatter.write_str("\nНе удалось разобрать строки: ")?;
            for (index, line) in self.bad_lines.iter().take(MAX_BAD_LINES).enumerate() {
                if index > 0 {
                    formatter.write_str(", ")?;
                }
                write!(formatter, "{line}")?;
            }
            if self.bad_lines.len() > MAX_BAD_LINES {
                formatter.write_str(" и другие")?;
            }
            formatter.write_str(".")?;
        }
        if self.summary.over_word_limit > 0 {
            write!(
                formatter,
                "\nНе добавлено пар: {} — в коллекции уже максимум слов ({}).",
                self.summary.over_word_limit, self.limits.max_words_per_collection
            )?;
        }
        if self.summary.over_translation_limit > 0 {
            write!(
                formatter,
                "\nНе добавлено пар: {} — у слова уже максимум переводов ({}).",
                self.summary.over_translation_limit, self.limits.max_translations_per_word
            )?;
        }
        Ok(())
    }
}

/// Text that replaces a quiz message after the user answers.
pub struct AnswerResult<'a> {
    is_correct: bool,
    term: &'a str,
    translations: &'a [String],
    chosen: &'a str,
}

impl<'a> AnswerResult<'a> {
    /// `None` when the quiz was already closed: there is nothing to show.
    pub fn new(outcome: &'a AnswerOutcome) -> Option<AnswerResult<'a>> {
        match outcome {
            AnswerOutcome::Answered {
                is_correct,
                term,
                translations,
                chosen,
            } => Some(AnswerResult {
                is_correct: *is_correct,
                term,
                translations,
                chosen,
            }),
            AnswerOutcome::Closed => None,
        }
    }
}

impl fmt::Display for AnswerResult<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = if self.is_correct { "✅" } else { "❌" };
        let mut output = Truncating::message(formatter);
        write!(
            output,
            "{mark} {} — {}",
            self.term,
            Joined(self.translations)
        )?;
        if !self.is_correct {
            write!(output, " (ты выбрал: {})", self.chosen)?;
        }
        output.finish()
    }
}

pub struct StatsReport<'a>(pub &'a CollectionStats);

impl fmt::Display for StatsReport<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stats = self.0;
        writeln!(formatter, "Слов в коллекции: {}", stats.word_count)?;
        if stats.answered == 0 {
            return formatter.write_str("Ответов пока нет.");
        }
        let percent = stats.correct * 100 / stats.answered;
        write!(
            formatter,
            "Верных ответов: {percent}% ({} из {})",
            stats.correct, stats.answered
        )?;
        if !stats.hardest.is_empty() {
            formatter.write_str("\n\nСамые трудные слова:")?;
            for word in &stats.hardest {
                write!(
                    formatter,
                    "\n{} — ошибок {:.0}%",
                    word.term,
                    word.error_rate * 100.0
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ids::WordId, storage::HardWord};

    #[test]
    fn full_page_of_long_words_shows_every_word() {
        let long = "щ".repeat(200);
        let words: Vec<_> = (0..WORDS_PAGE_SIZE)
            .map(|number| WordView {
                id: WordId(number),
                term: format!("слово{number}"),
                translations: vec![long.clone(); 20],
            })
            .collect();
        let text = WordsPage {
            words: &words,
            page: Page::new(i64::MAX),
            total_pages: i64::MAX,
        }
        .to_string();
        assert!(text.chars().count() <= 4096);
        for number in 0..WORDS_PAGE_SIZE {
            assert!(text.contains(&format!("\nслово{number} — ")), "{number}");
        }
    }

    #[test]
    fn text_over_telegram_limit_is_cut_with_ellipsis() {
        let fitting = "щ".repeat(4096);
        assert_eq!(Truncated(&fitting).to_string(), fitting);
        let outcome = AnswerOutcome::Answered {
            is_correct: false,
            term: "щ".repeat(3000),
            translations: vec!["ж".repeat(3000)],
            chosen: "ы".into(),
        };
        for text in [
            Truncated(&"щ".repeat(4097)).to_string(),
            AnswerResult::new(&outcome).unwrap().to_string(),
        ] {
            assert_eq!(text.chars().count(), 4096);
            assert!(text.ends_with('…'));
        }
    }

    #[test]
    fn short_words_page_is_not_truncated() {
        let words = [WordView {
            id: WordId(1),
            term: "run".into(),
            translations: vec!["бежать".into(), "управлять".into()],
        }];
        let text = WordsPage {
            words: &words,
            page: Page::FIRST,
            total_pages: 1,
        }
        .to_string();
        assert!(text.contains("run — бежать, управлять"));
        assert!(!text.contains('…'));
    }

    #[test]
    fn import_report_lists_first_20_bad_lines() {
        let bad: Vec<usize> = (1..=25).collect();
        let summary = ImportSummary {
            added: 2,
            updated: 1,
            ..ImportSummary::default()
        };
        let text = ImportReport {
            summary: &summary,
            bad_lines: &bad,
            limits: &LimitsConfig::default(),
        }
        .to_string();
        assert!(text.contains("20"));
        assert!(!text.contains("21"));
    }

    #[test]
    fn import_report_without_bad_lines_omits_them() {
        let summary = ImportSummary {
            added: 2,
            updated: 1,
            ..ImportSummary::default()
        };
        let text = ImportReport {
            summary: &summary,
            bad_lines: &[],
            limits: &LimitsConfig::default(),
        }
        .to_string();
        assert!(!text.contains("строки"));
        assert!(!text.contains("Не добавлено"));
    }

    #[test]
    fn import_report_names_pairs_over_limits() {
        let summary = ImportSummary {
            over_word_limit: 3,
            over_translation_limit: 2,
            ..ImportSummary::default()
        };
        let text = ImportReport {
            summary: &summary,
            bad_lines: &[],
            limits: &LimitsConfig::default(),
        }
        .to_string();
        assert!(text.contains("Не добавлено пар: 3 — в коллекции уже максимум слов (5000)."));
        assert!(text.contains("Не добавлено пар: 2 — у слова уже максимум переводов (20)."));
    }

    #[test]
    fn answer_result_shows_mark_translations_and_wrong_choice() {
        let translations = vec!["бежать".to_string(), "управлять".to_string()];
        let right = AnswerOutcome::Answered {
            is_correct: true,
            term: "run".into(),
            translations: translations.clone(),
            chosen: "бежать".into(),
        };
        let wrong = AnswerOutcome::Answered {
            is_correct: false,
            term: "run".into(),
            translations,
            chosen: "груша".into(),
        };
        assert_eq!(
            AnswerResult::new(&right).unwrap().to_string(),
            "✅ run — бежать, управлять"
        );
        assert_eq!(
            AnswerResult::new(&wrong).unwrap().to_string(),
            "❌ run — бежать, управлять (ты выбрал: груша)"
        );
        assert!(AnswerResult::new(&AnswerOutcome::Closed).is_none());
    }

    #[test]
    fn stats_report_shows_counts_and_hardest() {
        let stats = CollectionStats {
            word_count: 5,
            answered: 4,
            correct: 3,
            hardest: vec![HardWord {
                term: "run".into(),
                error_rate: 0.5,
            }],
        };
        let text = StatsReport(&stats).to_string();
        assert!(text.contains("Слов в коллекции: 5"));
        assert!(text.contains("75%"));
        assert!(text.contains("run — ошибок 50%"));
    }

    #[test]
    fn word_count_declines_in_russian() {
        for (count, expected) in [
            (1, "1 слово"),
            (2, "2 слова"),
            (3, "3 слова"),
            (5, "5 слов"),
            (11, "11 слов"),
            (21, "21 слово"),
            (22, "22 слова"),
        ] {
            assert_eq!(WordCount(count).to_string(), expected);
        }
    }

    #[test]
    fn help_names_minimum_word_count() {
        let help = Help {
            quiz_options: 4,
            paused: false,
        }
        .to_string();
        assert!(help.contains("минимум 4 слова с разными"));
        assert!(!help.contains("/resume."));
    }

    #[test]
    fn paused_help_names_resume() {
        let help = Help {
            quiz_options: 3,
            paused: true,
        }
        .to_string();
        assert!(help.starts_with("Квизы приостановлены"));
        assert!(help.contains("/resume"));
    }

    #[test]
    fn no_active_collection_hints_by_ownership() {
        let none = NoActiveCollection {
            has_collections: false,
        }
        .to_string();
        let others = NoActiveCollection {
            has_collections: true,
        }
        .to_string();
        assert!(none.contains("/collection new"));
        assert!(others.contains("Выбери"));
        assert!(!others.contains("/collection new"));
    }
}
