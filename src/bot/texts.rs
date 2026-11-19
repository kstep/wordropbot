use std::{
    fmt::{self, Write as _},
    slice,
};

use crate::{
    bot::page::Page,
    config::{LimitsConfig, MAX_MESSAGE_CHARS},
    storage::{AnswerOutcome, CollectionStats, ImportSummary, WordView},
};

pub const WORDS_PAGE_SIZE: u32 = 20;

const MAX_BAD_LINES: usize = 20;
/// Keeps a full page far below the rich message limit of 32768 characters.
const MAX_CELL_CHARS: usize = 200;

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

/// Russian noun form for a number of words: 1 слово, 2 слова, 5 слов, 11 слов,
/// 21 слово.
fn word_noun(count: usize) -> &'static str {
    match (count % 100, count % 10) {
        (11..=14, _) => "слов",
        (_, 1) => "слово",
        (_, 2..=4) => "слова",
        _ => "слов",
    }
}

/// The help message, which also names how many words a quiz needs.
pub fn help(quiz_options: usize, paused: bool) -> String {
    let mut text = String::new();
    if paused {
        text.push_str(PAUSED_NOTE);
        text.push_str("\n\n");
    }
    // Writing to a String cannot fail.
    let _ = write!(
        text,
        "{HELP}\n\nКвизы начнутся, когда в коллекции будет минимум {quiz_options} {} с разными переводами.",
        word_noun(quiz_options)
    );
    text
}

/// Added to an import reply when no word can be asked yet.
pub fn write_word_shortage_hint(output: &mut impl fmt::Write, quiz_options: usize) -> fmt::Result {
    write!(
        output,
        "Квизы пока не начнутся: добавь слов, чтобы в коллекции было минимум {quiz_options} {} с разными переводами.",
        word_noun(quiz_options)
    )
}

/// Reply to a command that needs an active collection when there is none.
pub fn missing_collection_hint(has_collections: bool) -> &'static str {
    if has_collections {
        "Активная коллекция не выбрана. Выбери её через /collection."
    } else {
        "Активной коллекции нет. Создай её командой /collection new <название> \
         или подключись по коду: /collection <КОД>."
    }
}

/// Writer that passes at most `max_chars` characters to `inner`: longer text
/// keeps its first `max_chars - 1` characters and ends with an ellipsis. Text
/// is written through as it comes, without buffering it.
pub struct LimitedWriter<W> {
    inner: W,
    max_chars: usize,
    written: usize,
    /// The character in the last slot, written only if nothing follows it.
    last: Option<char>,
    is_cut: bool,
}

impl<W: fmt::Write> LimitedWriter<W> {
    pub fn new(inner: W, max_chars: usize) -> LimitedWriter<W> {
        debug_assert!(max_chars > 0);
        LimitedWriter {
            inner,
            max_chars,
            written: 0,
            last: None,
            is_cut: false,
        }
    }

    /// Telegram's message limit.
    pub fn message(inner: W) -> LimitedWriter<W> {
        LimitedWriter::new(inner, MAX_MESSAGE_CHARS)
    }

    pub fn finish(mut self) -> fmt::Result {
        match self.last {
            Some(character) => self.inner.write_char(character),
            None => Ok(()),
        }
    }

    fn cut(&mut self) -> fmt::Result {
        self.is_cut = true;
        self.last = None;
        self.inner.write_char('…')
    }
}

impl<W: fmt::Write> fmt::Write for LimitedWriter<W> {
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

/// A message written through `write`, cut to Telegram's limit.
pub fn message_text(write: impl FnOnce(&mut LimitedWriter<&mut String>) -> fmt::Result) -> String {
    let mut text = String::new();
    let mut output = LimitedWriter::message(&mut text);
    // Writing to a String cannot fail.
    let _ = write(&mut output).and_then(|()| output.finish());
    text
}

fn write_joined(output: &mut impl fmt::Write, items: &[String]) -> fmt::Result {
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            output.write_str(", ")?;
        }
        output.write_str(item)?;
    }
    Ok(())
}

/// One page of the `/words` listing in rich HTML: a heading over a table of
/// terms and their translations.
pub fn words_html(words: &[WordView], page: Page, last: Page) -> String {
    let mut html = String::new();
    // Writing to a String cannot fail.
    let _ = write!(
        html,
        "<h3>Слова, страница {} из {}</h3>\
         <table><tr><th>Слово</th><th>Перевод</th></tr>",
        page.number(),
        last.number()
    );
    for word in words {
        html.push_str("<tr>");
        push_cell(&mut html, slice::from_ref(&word.term));
        push_cell(&mut html, &word.translations);
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    html
}

/// A table cell with `parts` joined by commas, escaped and cut at
/// `MAX_CELL_CHARS`.
fn push_cell(html: &mut String, parts: &[String]) {
    html.push_str("<td>");
    let mut characters = parts.iter().enumerate().flat_map(|(index, part)| {
        let separator = if index > 0 { ", " } else { "" };
        separator.chars().chain(part.chars())
    });
    for character in characters.by_ref().take(MAX_CELL_CHARS) {
        match character {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            _ => html.push(character),
        }
    }
    if characters.next().is_some() {
        html.push('…');
    }
    html.push_str("</td>");
}

impl ImportSummary {
    /// Reply to a text import.
    pub fn report(&self, bad_lines: &[usize], limits: &LimitsConfig) -> String {
        let mut text = format!(
            "Добавлено слов: {}, обновлено: {}.",
            self.added, self.updated
        );
        // Writing to a String cannot fail.
        if !bad_lines.is_empty() {
            text.push_str("\nНе удалось разобрать строки: ");
            for (index, line) in bad_lines.iter().take(MAX_BAD_LINES).enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                let _ = write!(text, "{line}");
            }
            if bad_lines.len() > MAX_BAD_LINES {
                text.push_str(" и другие");
            }
            text.push('.');
        }
        if self.over_word_limit > 0 {
            let _ = write!(
                text,
                "\nНе добавлено пар: {} — в коллекции уже максимум слов ({}).",
                self.over_word_limit, limits.max_words_per_collection
            );
        }
        if self.over_translation_limit > 0 {
            let _ = write!(
                text,
                "\nНе добавлено пар: {} — у слова уже максимум переводов ({}).",
                self.over_translation_limit, limits.max_translations_per_word
            );
        }
        text
    }
}

impl AnswerOutcome {
    /// Text that replaces the quiz message; `None` when the quiz was already
    /// closed.
    pub fn text(&self) -> Option<String> {
        let AnswerOutcome::Answered {
            is_correct,
            term,
            translations,
            chosen,
        } = self
        else {
            return None;
        };
        let mark = if *is_correct { "✅" } else { "❌" };
        Some(message_text(|output| {
            write!(output, "{mark} {term} — ")?;
            write_joined(output, translations)?;
            if !is_correct {
                write!(output, " (ты выбрал: {chosen})")?;
            }
            Ok(())
        }))
    }
}

impl CollectionStats {
    /// Reply to `/stats`.
    pub fn report(&self) -> String {
        let mut text = format!("Слов в коллекции: {}\n", self.word_count);
        if self.answered == 0 {
            text.push_str("Ответов пока нет.");
            return text;
        }
        let percent = self.correct * 100 / self.answered;
        // Writing to a String cannot fail.
        let _ = write!(
            text,
            "Верных ответов: {percent}% ({} из {})",
            self.correct, self.answered
        );
        if !self.hardest.is_empty() {
            text.push_str("\n\nСамые трудные слова:");
            for word in &self.hardest {
                let _ = write!(
                    text,
                    "\n{} — ошибок {:.0}%",
                    word.term,
                    word.error_rate * 100.0
                );
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ids::WordId, storage::HardWord};

    fn truncated(text: &str) -> String {
        message_text(|output| output.write_str(text))
    }

    #[test]
    fn words_html_escapes_and_lays_out_table() {
        let words = [
            WordView {
                id: WordId(1),
                term: "γράφω".into(),
                translations: vec!["я пишу".into()],
            },
            WordView {
                id: WordId(2),
                term: "a<b".into(),
                translations: vec!["x&y".into(), "z".into()],
            },
        ];
        assert_eq!(
            words_html(&words, Page::FIRST, Page::new(1)),
            "<h3>Слова, страница 1 из 2</h3>\
             <table><tr><th>Слово</th><th>Перевод</th></tr>\
             <tr><td>γράφω</td><td>я пишу</td></tr>\
             <tr><td>a&lt;b</td><td>x&amp;y, z</td></tr></table>"
        );
    }

    #[test]
    fn long_cells_are_cut() {
        let words: Vec<WordView> = (0..WORDS_PAGE_SIZE)
            .map(|number| WordView {
                id: WordId(i64::from(number)),
                term: format!("слово{number}"),
                translations: vec!["щ".repeat(200); 20],
            })
            .collect();
        let html = words_html(&words, Page::FIRST, Page::FIRST);
        let cell = format!("<td>{}…</td>", "щ".repeat(200));
        assert_eq!(html.matches(&cell).count(), words.len());
        for number in 0..WORDS_PAGE_SIZE {
            assert!(
                html.contains(&format!("<td>слово{number}</td>")),
                "{number}"
            );
        }
    }

    #[test]
    fn truncating_writer_matches_cut_of_whole_text() {
        let characters: Vec<char> = "аб".repeat(6).chars().collect();
        let whole: String = characters.iter().collect();
        for max_chars in 1..=characters.len() + 2 {
            let expected: String = if characters.len() > max_chars {
                characters[..max_chars - 1]
                    .iter()
                    .chain(['…'].iter())
                    .collect()
            } else {
                whole.clone()
            };
            for chunk_size in 1..=characters.len() + 1 {
                let mut text = String::new();
                let mut output = LimitedWriter::new(&mut text, max_chars);
                for chunk in characters.chunks(chunk_size) {
                    output.write_str("").unwrap();
                    output.write_str(&chunk.iter().collect::<String>()).unwrap();
                }
                output.finish().unwrap();
                assert_eq!(text, expected, "max {max_chars}, chunk {chunk_size}");
            }
        }
    }

    #[test]
    fn text_over_telegram_limit_is_cut_with_ellipsis() {
        let fitting = "щ".repeat(4096);
        assert_eq!(truncated(&fitting), fitting);
        let outcome = AnswerOutcome::Answered {
            is_correct: false,
            term: "щ".repeat(3000),
            translations: vec!["ж".repeat(3000)],
            chosen: "ы".into(),
        };
        for text in [truncated(&"щ".repeat(4097)), outcome.text().unwrap()] {
            assert_eq!(text.chars().count(), 4096);
            assert!(text.ends_with('…'));
        }
    }

    #[test]
    fn import_report_lists_first_20_bad_lines() {
        let bad: Vec<usize> = (1..=25).collect();
        let summary = ImportSummary {
            added: 2,
            updated: 1,
            ..ImportSummary::default()
        };
        let text = summary.report(&bad, &LimitsConfig::default());
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
        let text = summary.report(&[], &LimitsConfig::default());
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
        let text = summary.report(&[], &LimitsConfig::default());
        assert!(text.contains("Не добавлено пар: 3 — в коллекции уже максимум слов (5000)."));
        assert!(text.contains("Не добавлено пар: 2 — у слова уже максимум переводов (20)."));
    }

    #[test]
    fn answer_text_shows_mark_translations_and_wrong_choice() {
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
        assert_eq!(right.text().unwrap(), "✅ run — бежать, управлять");
        assert_eq!(
            wrong.text().unwrap(),
            "❌ run — бежать, управлять (ты выбрал: груша)"
        );
        assert!(AnswerOutcome::Closed.text().is_none());
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
        let text = stats.report();
        assert!(text.contains("Слов в коллекции: 5"));
        assert!(text.contains("75%"));
        assert!(text.contains("run — ошибок 50%"));
    }

    #[test]
    fn word_noun_declines_in_russian() {
        for (count, expected) in [
            (1, "слово"),
            (2, "слова"),
            (3, "слова"),
            (5, "слов"),
            (11, "слов"),
            (21, "слово"),
            (22, "слова"),
        ] {
            assert_eq!(word_noun(count), expected, "{count}");
        }
    }

    #[test]
    fn help_names_minimum_word_count() {
        let text = help(4, false);
        assert!(text.contains("минимум 4 слова с разными"));
        assert!(!text.contains("/resume."));
    }

    #[test]
    fn paused_help_names_resume() {
        let text = help(3, true);
        assert!(text.starts_with("Квизы приостановлены"));
        assert!(text.contains("/resume"));
    }

    #[test]
    fn no_active_collection_hints_by_ownership() {
        assert!(missing_collection_hint(false).contains("/collection new"));
        let others = missing_collection_hint(true);
        assert!(others.contains("Выбери"));
        assert!(!others.contains("/collection new"));
    }
}
