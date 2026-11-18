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
