use std::{fmt, str::FromStr};

use teloxide::types::InlineKeyboardButton;

use crate::{
    bot::page::Page,
    ids::{CollectionId, QuizId, WordId},
};

/// Button payload. Telegram caps `callback_data` at 64 bytes; the longest
/// form (`a:<i64>:<usize>`) stays well under that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackAction {
    Answer { quiz: QuizId, index: usize },
    Activate(CollectionId),
    LeaveAsk(CollectionId),
    LeaveConfirm(CollectionId),
    RemoveConfirm(WordId),
    WordsPage(Page),
    CollectionsPage(Page),
    Dismiss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("malformed callback data")]
pub struct ParseCallbackError;

impl CallbackAction {
    /// An inline button that sends this action back when pressed.
    pub fn button(self, label: impl Into<String>) -> InlineKeyboardButton {
        InlineKeyboardButton::callback(label, self.to_string())
    }
}

impl fmt::Display for CallbackAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallbackAction::Answer { quiz, index } => write!(formatter, "a:{quiz}:{index}"),
            CallbackAction::Activate(id) => write!(formatter, "c:{id}"),
            CallbackAction::LeaveAsk(id) => write!(formatter, "l:{id}"),
            CallbackAction::LeaveConfirm(id) => write!(formatter, "y:{id}"),
            CallbackAction::RemoveConfirm(id) => write!(formatter, "r:{id}"),
            CallbackAction::WordsPage(page) => write!(formatter, "w:{page}"),
            CallbackAction::CollectionsPage(page) => write!(formatter, "p:{page}"),
            CallbackAction::Dismiss => formatter.write_str("d"),
        }
    }
}

impl FromStr for CallbackAction {
    type Err = ParseCallbackError;

    fn from_str(data: &str) -> Result<CallbackAction, ParseCallbackError> {
        fn parse_part<T: FromStr>(text: &str) -> Result<T, ParseCallbackError> {
            text.parse().map_err(|_| ParseCallbackError)
        }
        let mut parts = data.split(':');
        let action = match (parts.next(), parts.next(), parts.next()) {
            (Some("a"), Some(quiz), Some(index)) => CallbackAction::Answer {
                quiz: QuizId(parse_part(quiz)?),
                index: parse_part(index)?,
            },
            (Some("c"), Some(id), None) => CallbackAction::Activate(CollectionId(parse_part(id)?)),
            (Some("l"), Some(id), None) => CallbackAction::LeaveAsk(CollectionId(parse_part(id)?)),
            (Some("y"), Some(id), None) => {
                CallbackAction::LeaveConfirm(CollectionId(parse_part(id)?))
            }
            (Some("r"), Some(id), None) => CallbackAction::RemoveConfirm(WordId(parse_part(id)?)),
            (Some("w"), Some(page), None) => CallbackAction::WordsPage(parse_part(page)?),
            (Some("p"), Some(page), None) => CallbackAction::CollectionsPage(parse_part(page)?),
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
            CallbackAction::WordsPage(Page::new(4)),
            CallbackAction::CollectionsPage(Page::new(5)),
            CallbackAction::Dismiss,
        ] {
            let text = action.to_string();
            assert!(text.len() <= 64, "{text}");
            assert_eq!(text.parse::<CallbackAction>(), Ok(action));
        }
    }

    #[test]
    fn rejects_garbage() {
        for data in [
            "", "x", "a:1", "a:x:1", "r:", "zz:1", "a:1:2:3", "c:1:2", "d:1",
        ] {
            assert_eq!(
                data.parse::<CallbackAction>(),
                Err(ParseCallbackError),
                "{data}"
            );
        }
    }
}
