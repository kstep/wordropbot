#[cfg(test)]
mod tests {
    use crate::import::ParsedPair;
    use crate::period::Period;
    use crate::storage::Storage;

    fn pair(t: &str, r: &str) -> ParsedPair {
        ParsedPair {
            term: t.into(),
            translation: r.into(),
        }
    }

    async fn setup_collection() -> (Storage, i64) {
        let s = Storage::in_memory().await;
        s.ensure_user(1, Period::from_secs(60).unwrap(), 0)
            .await
            .unwrap();
        let code = "K7QM-3XPA".parse().unwrap();
        let c = s
            .create_collection(1, "A", &code, 0)
            .await
            .unwrap()
            .unwrap()
            .id;
        (s, c)
    }

    #[tokio::test]
    async fn import_merges_case_insensitively() {
        let (s, c) = setup_collection().await;
        let r = s
            .import_pairs(
                c,
                &[
                    pair("Run", "бежать"),
                    pair("run", "управлять"),
                    pair("RUN", "Бежать"),
                ],
            )
            .await
            .unwrap();
        assert_eq!((r.added, r.updated), (1, 0));
        let w = s.find_word(c, "run").await.unwrap().unwrap();
        assert_eq!(w.term, "Run");
        assert_eq!(w.translations, ["бежать", "управлять"]);
    }

    #[tokio::test]
    async fn reimport_counts_only_real_updates() {
        let (s, c) = setup_collection().await;
        s.import_pairs(c, &[pair("run", "бежать")]).await.unwrap();
        let same = s.import_pairs(c, &[pair("run", "бежать")]).await.unwrap();
        assert_eq!((same.added, same.updated), (0, 0));
        let new = s.import_pairs(c, &[pair("run", "управлять")]).await.unwrap();
        assert_eq!((new.added, new.updated), (0, 1));
    }

    #[tokio::test]
    async fn same_term_in_two_collections_is_separate() {
        let (s, a) = setup_collection().await;
        let code = "AAAA-BBBB".parse().unwrap();
        let b = s
            .create_collection(1, "B", &code, 0)
            .await
            .unwrap()
            .unwrap()
            .id;
        s.import_pairs(a, &[pair("run", "бежать")]).await.unwrap();
        s.import_pairs(b, &[pair("run", "управлять")]).await.unwrap();
        assert_eq!(
            s.find_word(a, "run").await.unwrap().unwrap().translations,
            ["бежать"]
        );
    }

    #[tokio::test]
    async fn list_and_count_are_ordered_and_paged() {
        let (s, c) = setup_collection().await;
        s.import_pairs(c, &[pair("b", "2"), pair("A", "1"), pair("c", "3")])
            .await
            .unwrap();
        assert_eq!(s.count_words(c).await.unwrap(), 3);
        let page = s.list_words(c, 1, 2).await.unwrap();
        let terms: Vec<_> = page.iter().map(|w| w.term.as_str()).collect();
        assert_eq!(terms, ["b", "c"]);
        assert_eq!(page[0].translations, ["2"]);
    }

    #[tokio::test]
    async fn remove_requires_membership() {
        let (s, c) = setup_collection().await;
        s.ensure_user(2, Period::from_secs(60).unwrap(), 0)
            .await
            .unwrap();
        s.import_pairs(c, &[pair("run", "бежать")]).await.unwrap();
        let id = s.find_word(c, "run").await.unwrap().unwrap().id;
        assert!(s.remove_word(2, id, 0).await.unwrap().is_none());
        assert!(s.remove_word(1, id, 0).await.unwrap().is_some());
        assert!(s.find_word(c, "run").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn remove_cancels_open_quizzes_and_reschedules_owners() {
        let (s, c) = setup_collection().await;
        s.ensure_user(2, Period::from_secs(100).unwrap(), 0)
            .await
            .unwrap();
        s.join_collection(2, &"K7QM-3XPA".parse().unwrap(), 0)
            .await
            .unwrap();
        s.import_pairs(c, &[pair("run", "бежать")]).await.unwrap();
        let id = s.find_word(c, "run").await.unwrap().unwrap().id;
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, message_id, sent_at) \
             VALUES (2, ?, '[]', 0, 77, 0)",
        )
        .bind(id)
        .execute(&s.pool)
        .await
        .unwrap();
        let cancelled = s.remove_word(1, id, 1000).await.unwrap().unwrap();
        assert_eq!(
            cancelled,
            [crate::storage::CancelledQuiz {
                user_id: 2,
                message_id: Some(77)
            }]
        );
        assert_eq!(s.ensure_user(2, Period::from_secs(5).unwrap(), 0).await.unwrap().next_quiz_at, 1100);
        assert_eq!(s.ensure_user(1, Period::from_secs(5).unwrap(), 0).await.unwrap().next_quiz_at, 0);
    }
}
