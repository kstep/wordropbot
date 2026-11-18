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

/// SQLite support for a type stored as `$stored` and validated on the way out:
/// a stored value outside the invariant is a decode error, not a panic.
macro_rules! validated_sqlite_type {
    ($name:ty, $stored:ty, $decoded:ty, $convert:expr) => {
        impl sqlx::Type<sqlx::Sqlite> for $name {
            fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
                <$stored as sqlx::Type<sqlx::Sqlite>>::type_info()
            }

            fn compatible(info: &sqlx::sqlite::SqliteTypeInfo) -> bool {
                <$stored as sqlx::Type<sqlx::Sqlite>>::compatible(info)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for $name {
            fn decode(
                value: sqlx::sqlite::SqliteValueRef<'r>,
            ) -> Result<$name, sqlx::error::BoxDynError> {
                let stored = <$decoded as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
                Ok($convert(stored)?)
            }
        }
    };
}
pub(crate) use validated_sqlite_type;

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
