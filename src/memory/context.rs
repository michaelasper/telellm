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

        if let Some(reply_to) = &self.triggering_message.reply_to {
            output.push_str("\nReply context:\n");
            push_replied_message_line(&mut output, reply_to);
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
    if message.text.is_empty() {
        output.push_str("(no text)");
    } else {
        output.push_str(&message.text);
    }
    output.push('\n');
    push_attachments(output, &message.attachments);
    push_context_notes(output, &message.context_notes);
}

fn push_attachments(output: &mut String, attachments: &[crate::bot::message::IncomingAttachment]) {
    for attachment in attachments {
        output.push_str("  attachment: ");
        output.push_str(attachment.kind.as_str());
        if let Some(file_name) = &attachment.file_name {
            output.push_str(" `");
            output.push_str(file_name);
            output.push('`');
        }
        if let Some(mime_type) = &attachment.mime_type {
            output.push_str(" (");
            output.push_str(mime_type);
            output.push(')');
        }
        output.push_str(", ");
        output.push_str(&attachment.file_size.to_string());
        output.push_str(" bytes");
        if let Some(workspace_path) = &attachment.workspace_path {
            output.push_str(", available at @");
            output.push_str(workspace_path);
        } else if let Some(reason) = &attachment.skipped_reason {
            output.push_str(", not downloaded: ");
            output.push_str(reason);
        } else {
            output.push_str(", not downloaded");
        }
        output.push('\n');
    }
}

fn push_context_notes(output: &mut String, notes: &[String]) {
    for note in notes {
        output.push_str("  context: ");
        output.push_str(note);
        output.push('\n');
    }
}

fn push_replied_message_line(output: &mut String, message: &crate::bot::message::RepliedMessage) {
    let name = message.from_name.as_deref().unwrap_or("unknown");
    output.push_str("- ");
    output.push_str(name);
    output.push_str(": ");
    if message.text.is_empty() {
        output.push_str("(no text)");
    } else {
        output.push_str(&message.text);
    }
    output.push('\n');
    push_attachments(output, &message.attachments);
    push_context_notes(output, &message.context_notes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bot::message::{AttachmentKind, IncomingAttachment},
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
            attachments: Vec::new(),
            context_notes: Vec::new(),
            reply_to_bot: false,
            reply_to: None,
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

    #[test]
    fn render_should_include_replied_message_context() {
        let mut trigger = message("HUAC?");
        trigger.reply_to_bot = true;
        trigger.reply_to = Some(crate::bot::message::RepliedMessage {
            message_id: MessageId(7),
            from_name: Some("beru".to_owned()),
            text: "Of course I know about the commune, Nick.".to_owned(),
            attachments: Vec::new(),
            context_notes: Vec::new(),
        });
        let packet = ContextPacket {
            system_prompt: "Answer the trigger.".to_owned(),
            triggering_message: trigger,
            recent_messages: Vec::new(),
            memories: Vec::new(),
        };

        let rendered = packet.render();

        assert!(rendered.contains("Reply context:\n- beru: Of course I know about the commune"));
        assert!(rendered.contains("Triggering message:\n- Mike: HUAC?"));
    }

    #[test]
    fn render_should_include_context_notes() {
        let mut trigger = message("@telellm_bot");
        trigger.context_notes.push(
            "Audio transcript from @telegram_audio/msg-1/1-voice.ogg: hello there".to_owned(),
        );
        let packet = ContextPacket {
            system_prompt: "Answer the trigger.".to_owned(),
            triggering_message: trigger,
            recent_messages: Vec::new(),
            memories: Vec::new(),
        };

        let rendered = packet.render();

        assert!(rendered.contains("Audio transcript from @telegram_audio/msg-1/1-voice.ogg"));
        assert!(rendered.contains("hello there"));
    }

    #[test]
    fn render_should_include_attachment_workspace_paths() {
        let mut trigger = message("what is in this image?");
        trigger.attachments.push(IncomingAttachment {
            kind: AttachmentKind::Photo,
            file_id: "file-id".to_owned(),
            file_unique_id: "unique-id".to_owned(),
            file_name: Some("photo.jpg".to_owned()),
            mime_type: Some("image/jpeg".to_owned()),
            file_size: 123,
            workspace_path: Some("telegram_uploads/msg-1/1-photo.jpg".to_owned()),
            skipped_reason: None,
        });
        let packet = ContextPacket {
            system_prompt: "Answer the trigger.".to_owned(),
            triggering_message: trigger,
            recent_messages: Vec::new(),
            memories: Vec::new(),
        };

        let rendered = packet.render();

        assert!(rendered.contains("attachment: photo `photo.jpg` (image/jpeg), 123 bytes"));
        assert!(rendered.contains("available at @telegram_uploads/msg-1/1-photo.jpg"));
    }
}
