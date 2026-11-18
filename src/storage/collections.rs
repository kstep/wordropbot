use super::Storage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: i64,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionSummary {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub word_count: i64,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledQuiz {
    pub user_id: i64,
    pub message_id: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn join_makes_collection_active() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        s.ensure_user(2, 60, 0).await.unwrap();
        let c = s.create_collection(1, "Греческий", "K7QM-3XPA", 0).await.unwrap().unwrap();
        assert_eq!(s.join_collection(2, "K7QM-3XPA", 0).await.unwrap().unwrap().id, c.id);
        assert_eq!(s.get_user(2).await.unwrap().unwrap().active_collection_id, Some(c.id));
        assert!(s.join_collection(2, "AAAA-AAAA", 0).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_code_returns_none() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        s.create_collection(1, "A", "K7QM-3XPA", 0).await.unwrap().unwrap();
        assert!(s.create_collection(1, "B", "K7QM-3XPA", 0).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn last_member_leaving_deletes_collection() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        s.ensure_user(2, 60, 0).await.unwrap();
        let c = s.create_collection(1, "A", "K7QM-3XPA", 0).await.unwrap().unwrap();
        s.join_collection(2, "K7QM-3XPA", 0).await.unwrap();
        s.leave_collection(1, c.id, 0).await.unwrap().unwrap();
        assert!(s.join_collection(1, "K7QM-3XPA", 0).await.unwrap().is_some());
        s.leave_collection(1, c.id, 0).await.unwrap().unwrap();
        s.leave_collection(2, c.id, 0).await.unwrap().unwrap();
        assert!(s.join_collection(1, "K7QM-3XPA", 0).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn non_member_cannot_touch_collection() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        s.ensure_user(2, 60, 0).await.unwrap();
        let c = s.create_collection(1, "A", "K7QM-3XPA", 0).await.unwrap().unwrap();
        assert!(!s.set_active_collection(2, c.id).await.unwrap());
        assert!(s.leave_collection(2, c.id, 0).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejoin_keeps_membership_and_reactivates() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        let a = s.create_collection(1, "A", "K7QM-3XPA", 0).await.unwrap().unwrap();
        s.create_collection(1, "B", "AAAA-AAAA", 0).await.unwrap().unwrap();
        s.join_collection(1, "K7QM-3XPA", 5).await.unwrap().unwrap();
        assert_eq!(s.get_user(1).await.unwrap().unwrap().active_collection_id, Some(a.id));
        let list = s.list_collections(1).await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|c| c.id == a.id && c.is_active && c.word_count == 0));
    }

    #[tokio::test]
    async fn leaving_cancels_open_quiz_of_this_collection_only() {
        let s = Storage::in_memory().await;
        s.ensure_user(1, 60, 0).await.unwrap();
        let a = s.create_collection(1, "A", "K7QM-3XPA", 0).await.unwrap().unwrap();
        let b = s.create_collection(1, "B", "AAAA-AAAA", 0).await.unwrap().unwrap();
        sqlx::query("INSERT INTO words (id, collection_id, term, term_norm) VALUES (1, ?, 'a', 'a')")
            .bind(a.id)
            .execute(&s.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO quizzes (user_id, word_id, options, correct_index, message_id, sent_at) \
             VALUES (1, 1, '[]', 0, 42, 0)",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        // Leaving B (the active one) must not touch the quiz from A.
        let cancelled = s.leave_collection(1, b.id, 100).await.unwrap().unwrap();
        assert!(cancelled.is_empty());
        assert_eq!(s.get_user(1).await.unwrap().unwrap().active_collection_id, None);
        let cancelled = s.leave_collection(1, a.id, 100).await.unwrap().unwrap();
        assert_eq!(cancelled, [CancelledQuiz { user_id: 1, message_id: Some(42) }]);
        assert_eq!(s.get_user(1).await.unwrap().unwrap().next_quiz_at, 160);
    }
}
