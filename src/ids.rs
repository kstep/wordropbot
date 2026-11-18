use std::fmt::{self, Display};

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident($inner:ty)) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
        #[sqlx(transparent)]
        pub struct $name(pub $inner);

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                Display::fmt(&self.0, formatter)
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
