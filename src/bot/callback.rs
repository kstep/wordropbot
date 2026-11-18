use std::fmt;
use std::str::FromStr;

use crate::ids::{CollectionId, QuizId, WordId};

/// Button payload. Telegram caps `callback_data` at 64 bytes; the longest
/// form (`a:<i64>:<usize>`) stays well under that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackAction {
    Answer { quiz: QuizId, index: usize },
    Activate(CollectionId),
    LeaveAsk(CollectionId),
    LeaveConfirm(CollectionId),
    RemoveConfirm(WordId),
    WordsPage(i64),
    Dismiss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("malformed callback data")]
pub struct ParseCallbackError;

impl fmt::Display for CallbackAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallbackAction::Answer { quiz, index } => write!(f, "a:{quiz}:{index}"),
            CallbackAction::Activate(id) => write!(f, "c:{id}"),
            CallbackAction::LeaveAsk(id) => write!(f, "l:{id}"),
            CallbackAction::LeaveConfirm(id) => write!(f, "y:{id}"),
            CallbackAction::RemoveConfirm(id) => write!(f, "r:{id}"),
            CallbackAction::WordsPage(page) => write!(f, "w:{page}"),
            CallbackAction::Dismiss => f.write_str("d"),
        }
    }
}

impl FromStr for CallbackAction {
    type Err = ParseCallbackError;

    fn from_str(data: &str) -> Result<CallbackAction, ParseCallbackError> {
        fn num<T: FromStr>(s: &str) -> Result<T, ParseCallbackError> {
            s.parse().map_err(|_| ParseCallbackError)
        }
        let mut parts = data.split(':');
        let action = match (parts.next(), parts.next(), parts.next()) {
            (Some("a"), Some(quiz), Some(index)) => CallbackAction::Answer {
                quiz: QuizId(num(quiz)?),
                index: num(index)?,
            },
            (Some("c"), Some(id), None) => CallbackAction::Activate(CollectionId(num(id)?)),
            (Some("l"), Some(id), None) => CallbackAction::LeaveAsk(CollectionId(num(id)?)),
            (Some("y"), Some(id), None) => CallbackAction::LeaveConfirm(CollectionId(num(id)?)),
            (Some("r"), Some(id), None) => CallbackAction::RemoveConfirm(WordId(num(id)?)),
            (Some("w"), Some(page), None) => CallbackAction::WordsPage(num(page)?),
            (Some("d"), None, None) => CallbackAction::Dismiss,
            _ => return Err(ParseCallbackError),
        };
        if parts.next().is_some() {
            return Err(ParseCallbackError);
        }
        Ok(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_within_telegram_limit() {
        for action in [
            CallbackAction::Answer {
                quiz: QuizId(i64::MAX),
                index: 7,
            },
            CallbackAction::Activate(CollectionId(1)),
            CallbackAction::LeaveAsk(CollectionId(2)),
            CallbackAction::LeaveConfirm(CollectionId(3)),
            CallbackAction::RemoveConfirm(WordId(i64::MAX)),
            CallbackAction::WordsPage(4),
            CallbackAction::Dismiss,
        ] {
            let text = action.to_string();
            assert!(text.len() <= 64, "{text}");
            assert_eq!(text.parse::<CallbackAction>(), Ok(action));
        }
    }

    #[test]
    fn rejects_garbage() {
        for s in [
            "", "x", "a:1", "a:x:1", "r:", "zz:1", "a:1:2:3", "c:1:2", "d:1",
        ] {
            assert_eq!(s.parse::<CallbackAction>(), Err(ParseCallbackError), "{s}");
        }
    }
}
