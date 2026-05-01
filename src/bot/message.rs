use crate::ids::{ChatId, MessageId, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    pub chat_id: ChatId,
    pub message_id: MessageId,
    pub from: Option<UserId>,
    pub from_name: Option<String>,
    pub text: String,
    pub reply_to_bot: bool,
    pub private_chat: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressing {
    Ambient,
    Addressed,
}

impl IncomingMessage {
    pub fn addressing(&self, bot_username: &str) -> Addressing {
        if self.private_chat || self.reply_to_bot || mentions_bot(&self.text, bot_username) {
            Addressing::Addressed
        } else {
            Addressing::Ambient
        }
    }
}

fn mentions_bot(text: &str, bot_username: &str) -> bool {
    let mention = format!("@{}", bot_username.trim_start_matches('@'));
    text.split_whitespace()
        .any(|word| word.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '@') == mention)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id: ChatId(-100),
            message_id: MessageId(1),
            from: Some(UserId(9)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            reply_to_bot: false,
            private_chat: false,
        }
    }

    #[test]
    fn addressing_should_detect_bot_mention() {
        let msg = message("hey @telellm_bot what do you think?");

        assert_eq!(msg.addressing("telellm_bot"), Addressing::Addressed);
    }

    #[test]
    fn addressing_should_treat_plain_group_chat_as_ambient() {
        let msg = message("this is just group chat");

        assert_eq!(msg.addressing("telellm_bot"), Addressing::Ambient);
    }

    #[test]
    fn addressing_should_treat_reply_to_bot_as_addressed() {
        let mut msg = message("continue");
        msg.reply_to_bot = true;

        assert_eq!(msg.addressing("telellm_bot"), Addressing::Addressed);
    }

    #[test]
    fn addressing_should_treat_private_chat_as_addressed() {
        let mut msg = message("plain DM text");
        msg.private_chat = true;

        assert_eq!(msg.addressing("telellm_bot"), Addressing::Addressed);
    }
}
