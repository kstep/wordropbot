#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_within_telegram_limit() {
        for a in [
            CallbackAction::Answer { quiz: QuizId(i64::MAX), index: 7 },
            CallbackAction::Activate(CollectionId(1)),
            CallbackAction::LeaveAsk(CollectionId(2)),
            CallbackAction::LeaveConfirm(CollectionId(3)),
            CallbackAction::RemoveConfirm(WordId(i64::MAX)),
            CallbackAction::WordsPage(4),
            CallbackAction::Dismiss,
        ] {
            let s = a.to_string();
            assert!(s.len() <= 64, "{s}");
            assert_eq!(s.parse::<CallbackAction>(), Ok(a));
        }
    }

    #[test]
    fn rejects_garbage() {
        for s in ["", "x", "a:1", "a:x:1", "r:", "zz:1", "a:1:2:3", "c:1:2", "d:1"] {
            assert_eq!(s.parse::<CallbackAction>(), Err(ParseCallbackError), "{s}");
        }
    }
}
