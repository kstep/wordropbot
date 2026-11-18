use super::Storage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub period_secs: i64,
    pub paused: bool,
    pub next_quiz_at: i64,
    pub active_collection_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_user_is_due_immediately() {
        let s = Storage::in_memory().await;
        let u = s.ensure_user(1, 86400, 100).await.unwrap();
        assert_eq!(u.next_quiz_at, 100);
        assert_eq!(s.due_users(100).await.unwrap(), [1]);
        assert!(s.due_users(99).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ensure_user_keeps_existing_settings() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 86400, 100).await.unwrap();
        s.set_period(1, 60, 200).await.unwrap();
        assert_eq!(s.ensure_user(1, 86400, 300).await.unwrap().period_secs, 60);
    }

    #[tokio::test]
    async fn period_and_resume_reschedule() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 86400, 0).await.unwrap();
        assert!(!s.set_period(1, 60, 100).await.unwrap());
        assert_eq!(s.get_user(1).await.unwrap().unwrap().next_quiz_at, 160);
        s.set_paused(1, true, 110).await.unwrap();
        assert!(s.due_users(1000).await.unwrap().is_empty());
        s.set_paused(1, false, 200).await.unwrap();
        assert_eq!(s.get_user(1).await.unwrap().unwrap().next_quiz_at, 260);
    }

    #[tokio::test]
    async fn open_quiz_blocks_reschedule_and_due() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        sqlx::query("INSERT INTO collections (id, code, name, created_at) VALUES (1, 'C', 'C', 0)")
            .execute(&s.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO words (id, collection_id, term, term_norm) VALUES (1, 1, 'a', 'a')")
            .execute(&s.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, sent_at) \
             VALUES (1, 1, '[]', 0, 0)",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        assert!(s.due_users(1000).await.unwrap().is_empty());
        assert!(s.set_period(1, 30, 500).await.unwrap());
        assert_eq!(s.get_user(1).await.unwrap().unwrap().next_quiz_at, 0);
    }
}
