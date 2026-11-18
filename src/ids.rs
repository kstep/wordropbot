use std::fmt;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident($inner:ty)) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
        #[sqlx(transparent)]
        pub struct $name(pub $inner);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

id_type!(
    /// Telegram user id.
    UserId(i64)
);
id_type!(
    /// Row id of a collection.
    CollectionId(i64)
);
id_type!(
    /// Row id of a word.
    WordId(i64)
);
id_type!(
    /// Row id of a quiz.
    QuizId(i64)
);
id_type!(
    /// Telegram message id.
    MessageId(i32)
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_inner_value() {
        assert_eq!(UserId(-42).to_string(), "-42");
        assert_eq!(MessageId(7).to_string(), "7");
    }

    #[test]
    fn ids_are_ordered_by_value() {
        assert!(WordId(1) < WordId(2));
        assert_eq!(QuizId(5), QuizId(5));
    }
}
