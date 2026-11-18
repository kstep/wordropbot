use std::fmt::{self, Write as _};

use crate::storage::{AnswerOutcome, CollectionStats, ImportSummary, WordView};

pub const WORDS_PAGE_SIZE: i64 = 20;

/// Telegram's limit on message length, in characters.
const MAX_MESSAGE_CHARS: usize = 4096;
const MAX_BAD_LINES: usize = 20;

pub const HELP: &str = "\
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

/// Writes `text`, cutting it with an ellipsis if it exceeds Telegram's message limit.
fn write_truncated(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    match text.char_indices().nth(MAX_MESSAGE_CHARS) {
        Some(_) => {
            // Keep room for the ellipsis so the whole message fits the limit.
            let keep = text
                .char_indices()
                .nth(MAX_MESSAGE_CHARS - 1)
                .map_or(text.len(), |(i, _)| i);
            f.write_str(&text[..keep])?;
            f.write_str("…")
        }
        None => f.write_str(text),
    }
}

/// One page of the `/words` listing.
pub struct WordsPage<'a> {
    pub words: &'a [WordView],
    pub page: i64,
    pub total_pages: i64,
}

impl fmt::Display for WordsPage<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = format!("Слова, страница {} из {}:", self.page, self.total_pages);
        for word in self.words {
            // Writing to a String cannot fail.
            let _ = write!(text, "\n{} — {}", word.term, word.translations.join(", "));
        }
        write_truncated(f, &text)
    }
}

/// Result of a text import.
pub struct ImportReport<'a> {
    pub summary: &'a ImportSummary,
    pub bad_lines: &'a [usize],
}

impl fmt::Display for ImportReport<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Добавлено слов: {}, обновлено: {}.",
            self.summary.added, self.summary.updated
        )?;
        if !self.bad_lines.is_empty() {
            let shown: Vec<String> = self
                .bad_lines
                .iter()
                .take(MAX_BAD_LINES)
                .map(ToString::to_string)
                .collect();
            write!(f, "\nНе удалось разобрать строки: {}", shown.join(", "))?;
            if self.bad_lines.len() > MAX_BAD_LINES {
                f.write_str(" и другие")?;
            }
            f.write_str(".")?;
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
            AnswerOutcome::Answered { is_correct, term, translations, chosen } => {
                Some(AnswerResult {
                    is_correct: *is_correct,
                    term,
                    translations,
                    chosen,
                })
            }
            AnswerOutcome::Closed => None,
        }
    }
}

impl fmt::Display for AnswerResult<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = if self.is_correct { "✅" } else { "❌" };
        let mut text = format!("{mark} {} — {}", self.term, self.translations.join(", "));
        if !self.is_correct {
            // Writing to a String cannot fail.
            let _ = write!(text, " (ты выбрал: {})", self.chosen);
        }
        write_truncated(f, &text)
    }
}

pub struct StatsReport<'a>(pub &'a CollectionStats);

impl fmt::Display for StatsReport<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stats = self.0;
        writeln!(f, "Слов в коллекции: {}", stats.word_count)?;
        if stats.answered == 0 {
            return f.write_str("Ответов пока нет.");
        }
        let percent = stats.correct * 100 / stats.answered;
        write!(f, "Верных ответов: {percent}% ({} из {})", stats.correct, stats.answered)?;
        if !stats.hardest.is_empty() {
            f.write_str("\n\nСамые трудные слова:")?;
            for word in &stats.hardest {
                write!(f, "\n{} — ошибок {:.0}%", word.term, word.error_rate * 100.0)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WordId;
    use crate::storage::HardWord;

    #[test]
    fn words_page_fits_telegram_limit() {
        let long = "щ".repeat(200);
        let words: Vec<_> = (0..WORDS_PAGE_SIZE)
            .map(|i| WordView {
                id: WordId(i),
                term: long.clone(),
                translations: vec![long.clone(); 50],
            })
            .collect();
        let text = WordsPage { words: &words, page: 1, total_pages: 3 }.to_string();
        assert_eq!(text.chars().count(), 4096);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn short_words_page_is_not_truncated() {
        let words = [WordView {
            id: WordId(1),
            term: "run".into(),
            translations: vec!["бежать".into(), "управлять".into()],
        }];
        let text = WordsPage { words: &words, page: 1, total_pages: 1 }.to_string();
        assert!(text.contains("run — бежать, управлять"));
        assert!(!text.contains('…'));
    }

    #[test]
    fn import_report_lists_first_20_bad_lines() {
        let bad: Vec<usize> = (1..=25).collect();
        let summary = ImportSummary { added: 2, updated: 1 };
        let text = ImportReport { summary: &summary, bad_lines: &bad }.to_string();
        assert!(text.contains("20"));
        assert!(!text.contains("21"));
    }

    #[test]
    fn import_report_without_bad_lines_omits_them() {
        let summary = ImportSummary { added: 2, updated: 1 };
        let text = ImportReport { summary: &summary, bad_lines: &[] }.to_string();
        assert!(!text.contains("строки"));
    }

    #[test]
    fn answer_result_follows_spec() {
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
    fn long_answer_result_fits_telegram_limit() {
        let outcome = AnswerOutcome::Answered {
            is_correct: false,
            term: "щ".repeat(3000),
            translations: vec!["ж".repeat(3000)],
            chosen: "ы".into(),
        };
        let text = AnswerResult::new(&outcome).unwrap().to_string();
        assert_eq!(text.chars().count(), 4096);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn stats_report_shows_counts_and_hardest() {
        let stats = CollectionStats {
            word_count: 5,
            answered: 4,
            correct: 3,
            hardest: vec![HardWord { term: "run".into(), error_rate: 0.5 }],
        };
        let text = StatsReport(&stats).to_string();
        assert!(text.contains("Слов в коллекции: 5"));
        assert!(text.contains("75%"));
        assert!(text.contains("run — ошибок 50%"));
    }

    #[test]
    fn help_documents_every_command() {
        for cmd in [
            "/collection", "/words", "/remove", "/stats", "/period", "/pause", "/resume", "/quiz",
            "/skip", "/help",
        ] {
            assert!(HELP.contains(cmd), "{cmd}");
        }
    }
}
