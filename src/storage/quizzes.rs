#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::Code;
    use crate::config::SelectionConfig;
    use crate::import::ParsedPair;
    use crate::period::Period;
    use crate::storage::Storage;

    const PERIOD: i64 = 600;

    struct Fixture {
        s: Storage,
        word: i64,
        collection: i64,
        code: Code,
        opts: Vec<String>,
    }

    fn pair(t: &str, r: &str) -> ParsedPair {
        ParsedPair {
            term: t.into(),
            translation: r.into(),
        }
    }

    fn cfg() -> SelectionConfig {
        SelectionConfig {
            half_life_days: 14.0,
            prior_error: 0.5,
            prior_strength: 1.0,
            min_weight: 0.05,
        }
    }

    async fn fixture() -> Fixture {
        let s = Storage::in_memory().await;
        let period = Period::from_secs(PERIOD as u64).unwrap();
        s.ensure_user(1, period, 0).await.unwrap();
        s.ensure_user(2, period, 0).await.unwrap();
        let code: Code = "K7QM-3XPA".parse().unwrap();
        let collection = s
            .create_collection(1, "A", &code, 0)
            .await
            .unwrap()
            .unwrap()
            .id;
        s.join_collection(2, &code, 0).await.unwrap().unwrap();
        s.import_pairs(
            collection,
            &[pair("run", "бежать"), pair("run", "управлять")],
        )
        .await
        .unwrap();
        let word = s.find_word(collection, "run").await.unwrap().unwrap().id;
        let opts = ["бежать", "груша", "стол"].map(String::from).to_vec();
        Fixture {
            s,
            word,
            collection,
            code,
            opts,
        }
    }

    #[tokio::test]
    async fn only_one_open_quiz() {
        let f = fixture().await;
        assert!(
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            f.s.create_quiz(1, f.word, &f.opts, 0, 11)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn answer_counts_once_and_reschedules() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            f.s.answer_quiz(q, 1, 0, 100).await.unwrap(),
            AnswerOutcome::Answered {
                is_correct: true,
                ..
            }
        ));
        assert!(matches!(
            f.s.answer_quiz(q, 1, 1, 101).await.unwrap(),
            AnswerOutcome::Closed
        ));
        assert_eq!(
            f.s.get_user(1).await.unwrap().unwrap().next_quiz_at,
            100 + PERIOD
        );
        let c = f.s.candidates(1).await.unwrap();
        assert_eq!(
            c.iter()
                .find(|c| c.word_id == f.word)
                .unwrap()
                .answers
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn answer_reports_word_and_choice() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        let AnswerOutcome::Answered {
            is_correct,
            term,
            translations,
            chosen,
        } = f.s.answer_quiz(q, 1, 1, 100).await.unwrap()
        else {
            panic!("quiz must be open");
        };
        assert!(!is_correct);
        assert_eq!(term, "run");
        assert_eq!(translations, ["бежать", "управлять"]);
        assert_eq!(chosen, "груша");
    }

    #[tokio::test]
    async fn stranger_cannot_answer() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            f.s.answer_quiz(q, 2, 0, 100).await.unwrap(),
            AnswerOutcome::Closed
        ));
        assert!(f.s.open_quiz(1).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn out_of_range_choice_is_rejected() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            f.s.answer_quiz(q, 1, 99, 100).await.unwrap(),
            AnswerOutcome::Closed
        ));
        assert!(f.s.open_quiz(1).await.unwrap().is_some());
        assert_eq!(f.s.get_user(1).await.unwrap().unwrap().next_quiz_at, 0);
    }

    #[tokio::test]
    async fn skip_leaves_no_history() {
        let f = fixture().await;
        f.s.create_quiz(1, f.word, &f.opts, 0, 10)
            .await
            .unwrap()
            .unwrap();
        assert!(f.s.skip_quiz(1, 100).await.unwrap().is_some());
        assert!(f.s.skip_quiz(1, 100).await.unwrap().is_none());
        assert_eq!(
            f.s.get_user(1).await.unwrap().unwrap().next_quiz_at,
            100 + PERIOD
        );
        assert!(
            f.s.candidates(1)
                .await
                .unwrap()
                .iter()
                .all(|c| c.answers.is_empty())
        );
    }

    #[tokio::test]
    async fn skip_without_quiz_changes_nothing() {
        let f = fixture().await;
        assert!(f.s.skip_quiz(1, 100).await.unwrap().is_none());
        assert_eq!(f.s.get_user(1).await.unwrap().unwrap().next_quiz_at, 0);
    }

    #[tokio::test]
    async fn removing_word_cancels_other_members_quiz() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(2, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.remove_word(1, f.word, 100).await.unwrap().unwrap();
        assert!(f.s.open_quiz(2).await.unwrap().is_none());
        assert!(matches!(
            f.s.answer_quiz(q, 2, 0, 101).await.unwrap(),
            AnswerOutcome::Closed
        ));
        assert_eq!(
            f.s.get_user(2).await.unwrap().unwrap().next_quiz_at,
            100 + PERIOD
        );
    }

    #[tokio::test]
    async fn leaving_keeps_history_and_cancels_open_quiz() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(2, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.answer_quiz(q, 2, 0, 20).await.unwrap();
        f.s.create_quiz(2, f.word, &f.opts, 0, 30)
            .await
            .unwrap()
            .unwrap();
        f.s.leave_collection(2, f.collection, 40)
            .await
            .unwrap()
            .unwrap();
        assert!(f.s.open_quiz(2).await.unwrap().is_none());
        assert!(f.s.candidates(2).await.unwrap().is_empty());
        f.s.join_collection(2, &f.code, 50).await.unwrap().unwrap();
        let c = f.s.candidates(2).await.unwrap();
        assert_eq!(
            c.iter()
                .find(|c| c.word_id == f.word)
                .unwrap()
                .answers
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn startup_cleanup_removes_only_unsent() {
        let f = fixture().await;
        let sent =
            f.s.create_quiz(1, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.set_quiz_message(sent, 77).await.unwrap();
        f.s.create_quiz(2, f.word, &f.opts, 0, 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f.s.delete_unsent_quizzes().await.unwrap(), 1);
        assert!(f.s.open_quiz(1).await.unwrap().is_some());
        assert!(f.s.open_quiz(2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn open_quiz_round_trips_options_and_message() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(1, f.word, &f.opts, 2, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.set_quiz_message(q, 5).await.unwrap();
        let open = f.s.open_quiz(1).await.unwrap().unwrap();
        assert_eq!(open.id, q);
        assert_eq!(open.term, "run");
        assert_eq!(open.options, f.opts);
        assert_eq!(open.correct_index, 2);
        assert_eq!(open.message_id, Some(5));
        f.s.delete_quiz(q).await.unwrap();
        assert!(f.s.open_quiz(1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn malformed_options_are_a_decode_error() {
        let f = fixture().await;
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, sent_at) \
             VALUES (1, ?, 'not json', 0, 0)",
        )
        .bind(f.word)
        .execute(&f.s.pool)
        .await
        .unwrap();
        assert!(matches!(
            f.s.open_quiz(1).await,
            Err(sqlx::Error::Decode(_))
        ));
    }

    #[tokio::test]
    async fn candidates_include_unanswered_words_with_ordered_translations() {
        let f = fixture().await;
        let c = f.s.candidates(1).await.unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].collection_id, f.collection);
        assert_eq!(c[0].term, "run");
        assert_eq!(c[0].translations, ["бежать", "управлять"]);
        assert!(c[0].answers.is_empty());
    }

    #[tokio::test]
    async fn candidates_hold_only_own_answers() {
        let f = fixture().await;
        let q =
            f.s.create_quiz(2, f.word, &f.opts, 0, 10)
                .await
                .unwrap()
                .unwrap();
        f.s.answer_quiz(q, 2, 0, 20).await.unwrap();
        assert!(f.s.candidates(1).await.unwrap()[0].answers.is_empty());
        assert_eq!(f.s.candidates(2).await.unwrap()[0].answers.len(), 1);
    }

    #[tokio::test]
    async fn stats_rank_hardest_words_with_answers_only() {
        let f = fixture().await;
        f.s.import_pairs(
            f.collection,
            &[pair("b", "2"), pair("a", "1"), pair("unseen", "3")],
        )
        .await
        .unwrap();
        let mut now = 1000;
        // `run`: wrong; `a` and `b`: wrong, then tied on the same error rate; `unseen`: no answers.
        for (term, choice) in [("run", 1), ("a", 1), ("b", 1)] {
            let w = f.s.find_word(f.collection, term).await.unwrap().unwrap().id;
            let q =
                f.s.create_quiz(1, w, &f.opts, 0, now)
                    .await
                    .unwrap()
                    .unwrap();
            f.s.answer_quiz(q, 1, choice, now).await.unwrap();
            now += 1;
        }
        let w =
            f.s.find_word(f.collection, "unseen")
                .await
                .unwrap()
                .unwrap()
                .id;
        let q =
            f.s.create_quiz(1, w, &f.opts, 0, now)
                .await
                .unwrap()
                .unwrap();
        f.s.answer_quiz(q, 1, 0, now).await.unwrap();
        let stats =
            f.s.collection_stats(1, f.collection, now, &cfg())
                .await
                .unwrap();
        assert_eq!(stats.word_count, 4);
        assert_eq!((stats.answered, stats.correct), (4, 1));
        let terms: Vec<_> = stats.hardest.iter().map(|(t, _)| t.as_str()).collect();
        // Older wrong answers weigh slightly less, so the ordering is by rate and
        // then, for equal rates, by term; `unseen` has the lowest rate.
        assert_eq!(terms.len(), 4);
        assert_eq!(terms.last(), Some(&"unseen"));
        assert!(stats.hardest.windows(2).all(|p| p[0].1 >= p[1].1));
    }

    #[tokio::test]
    async fn stats_cap_hardest_at_ten_and_skip_unanswered() {
        let f = fixture().await;
        let pairs: Vec<_> = (0..12).map(|i| pair(&format!("w{i:02}"), "x")).collect();
        f.s.import_pairs(f.collection, &pairs).await.unwrap();
        for i in 0..11 {
            let w =
                f.s.find_word(f.collection, &format!("w{i:02}"))
                    .await
                    .unwrap()
                    .unwrap()
                    .id;
            let q = f.s.create_quiz(1, w, &f.opts, 0, 5).await.unwrap().unwrap();
            f.s.answer_quiz(q, 1, 1, 5).await.unwrap();
        }
        let stats =
            f.s.collection_stats(1, f.collection, 5, &cfg())
                .await
                .unwrap();
        assert_eq!(stats.word_count, 14);
        assert_eq!(stats.hardest.len(), 10);
        // Equal rates: the tie is broken by term, so the first ten come back.
        assert_eq!(stats.hardest[0].0, "w00");
        assert_eq!(stats.hardest[9].0, "w09");
    }
}
