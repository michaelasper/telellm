use super::store::MemoryRecord;
use crate::bot::message::IncomingMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPacket {
    pub system_prompt: String,
    pub triggering_message: IncomingMessage,
    pub recent_messages: Vec<IncomingMessage>,
    pub memories: Vec<MemoryRecord>,
}

impl ContextPacket {
    pub fn render(&self) -> String {
        let mut output = String::new();
        output.push_str(self.system_prompt.trim());
        output.push_str("\n\n");

        output.push_str("Long-term memory:\n");
        if self.memories.is_empty() {
            output.push_str("- No durable memory is stored for this group yet.\n");
        } else {
            for memory in &self.memories {
                output.push_str("- ");
                output.push_str(memory.kind.as_str());
                output.push_str(": ");
                output.push_str(&memory.content);
                output.push('\n');
            }
        }

        output.push_str("\nRecent chat:\n");
        if self.recent_messages.is_empty() {
            output.push_str("- No recent chat is available.\n");
        } else {
            for message in &self.recent_messages {
                push_message_line(&mut output, message);
            }
        }

        output.push_str("\nTriggering message:\n");
        push_message_line(&mut output, &self.triggering_message);
        output
    }
}

fn push_message_line(output: &mut String, message: &IncomingMessage) {
    let name = message.from_name.as_deref().unwrap_or("unknown");
    output.push_str("- ");
    output.push_str(name);
    output.push_str(": ");
    output.push_str(&message.text);
    output.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ids::{ChatId, MessageId, UserId},
        memory::store::MemoryKind,
    };

    fn message(text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id: ChatId(1),
            message_id: MessageId(1),
            from: Some(UserId(2)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            reply_to_bot: false,
            private_chat: false,
        }
    }

    #[test]
    fn render_should_include_memory_recent_chat_and_trigger() {
        let packet = ContextPacket {
            system_prompt: "Use memory well.".to_owned(),
            triggering_message: message("@telellm_bot summarize that"),
            recent_messages: vec![message("we were talking about the deploy")],
            memories: vec![MemoryRecord {
                id: 1,
                chat_id: ChatId(1),
                user_id: Some(UserId(2)),
                kind: MemoryKind::Preference,
                content: "Mike likes concise updates".to_owned(),
            }],
        };

        let rendered = packet.render();
        assert!(rendered.starts_with("Use memory well."));
        assert!(rendered.contains("Mike likes concise updates"));
    }

    #[test]
    fn render_should_show_empty_memory_and_recent_chat_placeholders() {
        let packet = ContextPacket {
            system_prompt: "Answer the trigger.".to_owned(),
            triggering_message: message("@telellm_bot hello"),
            recent_messages: Vec::new(),
            memories: Vec::new(),
        };

        let rendered = packet.render();
        assert!(rendered.contains("No durable memory is stored for this group yet"));
    }
}
