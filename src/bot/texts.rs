use std::fmt::{self, Write as _};

use crate::storage::{AnswerOutcome, CollectionStats, ImportSummary, WordView};

pub const WORDS_PAGE_SIZE: i64 = 20;

/// Telegram's limit on message length, in characters.
const MAX_MESSAGE_CHARS: usize = 4096;
const MAX_BAD_LINES: usize = 20;

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

const PAUSED_NOTE: &str = "Квизы приостановлены. Возобновить: /resume.";

/// The help message, which also names how many words a quiz needs.
pub struct Help {
    /// `quiz.options`: a quiz needs that many words with different translations.
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
            "{HELP}\n\nКвизы начнутся, когда в коллекции будет минимум {} слов с разными переводами.",
            self.quiz_options
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
            "Квизы пока не начнутся: добавь слов, чтобы в коллекции было минимум {} слов с разными переводами.",
            self.quiz_options
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

/// Text cut with an ellipsis if it exceeds Telegram's message limit.
struct Truncated<'a>(&'a str);

impl fmt::Display for Truncated<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.0;
        match text.char_indices().nth(MAX_MESSAGE_CHARS) {
            Some(_) => {
                // Keep room for the ellipsis so the whole message fits the limit.
                let keep = text
                    .char_indices()
                    .nth(MAX_MESSAGE_CHARS - 1)
                    .map_or(text.len(), |(i, _)| i);
                formatter.write_str(&text[..keep])?;
                formatter.write_str("…")
            }
            None => formatter.write_str(text),
        }
    }
}

/// One page of the `/words` listing.
pub struct WordsPage<'a> {
    pub words: &'a [WordView],
    pub page: i64,
    pub total_pages: i64,
}

impl fmt::Display for WordsPage<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = format!("Слова, страница {} из {}:", self.page, self.total_pages);
        for word in self.words {
            // Writing to a String cannot fail.
            let _ = write!(text, "\n{} — {}", word.term, word.translations.join(", "));
        }
        Truncated(&text).fmt(formatter)
    }
}

/// Result of a text import.
pub struct ImportReport<'a> {
    pub summary: &'a ImportSummary,
    pub bad_lines: &'a [usize],
}

impl fmt::Display for ImportReport<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
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
            write!(
                formatter,
                "\nНе удалось разобрать строки: {}",
                shown.join(", ")
            )?;
            if self.bad_lines.len() > MAX_BAD_LINES {
                formatter.write_str(" и другие")?;
            }
            formatter.write_str(".")?;
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
        let mut text = format!("{mark} {} — {}", self.term, self.translations.join(", "));
        if !self.is_correct {
            // Writing to a String cannot fail.
            let _ = write!(text, " (ты выбрал: {})", self.chosen);
        }
        Truncated(&text).fmt(formatter)
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
        let text = WordsPage {
            words: &words,
            page: 1,
            total_pages: 3,
        }
        .to_string();
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
        let text = WordsPage {
            words: &words,
            page: 1,
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
        };
        let text = ImportReport {
            summary: &summary,
            bad_lines: &bad,
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
        };
        let text = ImportReport {
            summary: &summary,
            bad_lines: &[],
        }
        .to_string();
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
    fn help_documents_every_command() {
        let help = Help {
            quiz_options: 3,
            paused: false,
        }
        .to_string();
        for command in [
            "/collection",
            "/words",
            "/remove",
            "/stats",
            "/period",
            "/pause",
            "/resume",
            "/quiz",
            "/skip",
            "/help",
        ] {
            assert!(help.contains(command), "{command}");
        }
    }

    #[test]
    fn help_names_minimum_word_count() {
        let help = Help {
            quiz_options: 4,
            paused: false,
        }
        .to_string();
        assert!(help.contains("минимум 4 слов"));
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
    fn not_enough_words_names_minimum() {
        let text = NotEnoughWords { quiz_options: 5 }.to_string();
        assert!(text.contains("минимум 5 слов"));
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
