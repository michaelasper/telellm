use crate::memory::context::ContextPacket;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptEnvelope {
    body: String,
}

impl PromptEnvelope {
    pub fn from_context(packet: &ContextPacket) -> Self {
        Self {
            body: packet.render(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bot::message::IncomingMessage,
        ids::{ChatId, MessageId, UserId},
        memory::context::ContextPacket,
    };

    #[test]
    fn from_context_should_render_triggering_message() {
        let message = IncomingMessage {
            chat_id: ChatId(1),
            message_id: MessageId(1),
            from: Some(UserId(2)),
            from_name: Some("Mike".to_owned()),
            text: "@telellm_bot hello".to_owned(),
            attachments: Vec::new(),
            context_notes: Vec::new(),
            reply_to_bot: false,
            reply_to: None,
            private_chat: false,
        };
        let packet = ContextPacket {
            system_prompt: "System prompt.".to_owned(),
            triggering_message: message,
            recent_messages: Vec::new(),
            memories: Vec::new(),
        };

        let envelope = PromptEnvelope::from_context(&packet);
        assert!(envelope.as_str().contains("@telellm_bot hello"));
    }
}
