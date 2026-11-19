//! Rich messages (Bot API 10.1), which teloxide does not wrap yet.

use serde::{Serialize, de::IgnoredAny};
use teloxide::{
    requests::Payload,
    types::{ChatId, InlineKeyboardMarkup, MessageId},
};

/// Rich content written in Telegram's rich HTML style.
#[derive(Clone, Debug, Serialize)]
struct RichHtml {
    html: String,
}

/// `sendRichMessage`.
#[derive(Clone, Debug, Serialize)]
pub struct SendRichMessage {
    chat_id: ChatId,
    rich_message: RichHtml,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<InlineKeyboardMarkup>,
}

impl SendRichMessage {
    pub fn new(
        chat_id: ChatId,
        html: String,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> SendRichMessage {
        SendRichMessage {
            chat_id,
            rich_message: RichHtml { html },
            reply_markup,
        }
    }
}

impl Payload for SendRichMessage {
    type Output = IgnoredAny;

    const NAME: &'static str = "sendRichMessage";
}

/// `editMessageText` with rich content instead of text.
#[derive(Clone, Debug, Serialize)]
pub struct EditRichMessage {
    chat_id: ChatId,
    #[serde(flatten)]
    message_id: MessageId,
    rich_message: RichHtml,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<InlineKeyboardMarkup>,
}

impl EditRichMessage {
    pub fn new(
        chat_id: ChatId,
        message_id: MessageId,
        html: String,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> EditRichMessage {
        EditRichMessage {
            chat_id,
            message_id,
            rich_message: RichHtml { html },
            reply_markup,
        }
    }
}

impl Payload for EditRichMessage {
    type Output = IgnoredAny;

    const NAME: &'static str = "editMessageText";
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn requests_match_bot_api() {
        let send = SendRichMessage::new(ChatId(7), "<b>x</b>".into(), None);
        assert_eq!(
            serde_json::to_value(&send).unwrap(),
            json!({"chat_id": 7, "rich_message": {"html": "<b>x</b>"}})
        );
        let keyboard = InlineKeyboardMarkup::new([[]]);
        let edit = EditRichMessage::new(ChatId(7), MessageId(5), "y".into(), Some(keyboard));
        assert_eq!(
            serde_json::to_value(&edit).unwrap(),
            json!({
                "chat_id": 7,
                "message_id": 5,
                "rich_message": {"html": "y"},
                "reply_markup": {"inline_keyboard": [[]]},
            })
        );
    }
}
