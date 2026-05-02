use crate::ids::{ChatId, MessageId, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    pub chat_id: ChatId,
    pub message_id: MessageId,
    pub from: Option<UserId>,
    pub from_name: Option<String>,
    pub text: String,
    pub attachments: Vec<IncomingAttachment>,
    pub reply_to_bot: bool,
    pub reply_to: Option<RepliedMessage>,
    pub private_chat: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingAttachment {
    pub kind: AttachmentKind,
    pub file_id: String,
    pub file_unique_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: u64,
    pub workspace_path: Option<String>,
    pub skipped_reason: Option<String>,
}

impl IncomingAttachment {
    pub fn new(
        kind: AttachmentKind,
        file_id: String,
        file_unique_id: String,
        file_name: Option<String>,
        mime_type: Option<String>,
        file_size: u64,
    ) -> Self {
        Self {
            kind,
            file_id,
            file_unique_id,
            file_name,
            mime_type,
            file_size,
            workspace_path: None,
            skipped_reason: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Photo,
    Document,
    Voice,
    Audio,
}

impl AttachmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Document => "document",
            Self::Voice => "voice",
            Self::Audio => "audio",
        }
    }

    pub fn default_file_name(self) -> &'static str {
        match self {
            Self::Photo => "photo.jpg",
            Self::Document => "document",
            Self::Voice => "voice.ogg",
            Self::Audio => "audio",
        }
    }

    pub fn is_audio(self) -> bool {
        matches!(self, Self::Voice | Self::Audio)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepliedMessage {
    pub message_id: MessageId,
    pub from_name: Option<String>,
    pub text: String,
    pub attachments: Vec<IncomingAttachment>,
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
    text.split_whitespace().any(|word| {
        word.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '@')
            .eq_ignore_ascii_case(&mention)
    })
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
            attachments: Vec::new(),
            reply_to_bot: false,
            reply_to: None,
            private_chat: false,
        }
    }

    #[test]
    fn addressing_should_detect_bot_mention() {
        let msg = message("hey @telellm_bot what do you think?");

        assert_eq!(msg.addressing("telellm_bot"), Addressing::Addressed);
    }

    #[test]
    fn addressing_should_detect_bot_mention_case_insensitively() {
        let msg = message("hey @TeleLLM_Bot what do you think?");

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

    #[test]
    fn attachment_kind_should_identify_audio() {
        assert!(AttachmentKind::Voice.is_audio());
        assert!(AttachmentKind::Audio.is_audio());
        assert!(!AttachmentKind::Photo.is_audio());
        assert!(!AttachmentKind::Document.is_audio());
    }

    #[test]
    fn attachment_kind_should_name_audio_defaults() {
        assert_eq!(AttachmentKind::Voice.as_str(), "voice");
        assert_eq!(AttachmentKind::Audio.as_str(), "audio");
        assert_eq!(AttachmentKind::Voice.default_file_name(), "voice.ogg");
        assert_eq!(AttachmentKind::Audio.default_file_name(), "audio");
    }
}
