use crate::{bot::message::IncomingMessage, ids::ChatId};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone)]
pub struct RollingBuffer {
    capacity: usize,
    messages: HashMap<ChatId, VecDeque<IncomingMessage>>,
}

impl RollingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            messages: HashMap::new(),
        }
    }

    pub fn push(&mut self, message: IncomingMessage) {
        let queue = self.messages.entry(message.chat_id).or_default();
        queue.push_back(message);
        while queue.len() > self.capacity {
            queue.pop_front();
        }
    }

    pub fn recent_for_chat(&self, chat_id: ChatId) -> Vec<IncomingMessage> {
        self.messages
            .get(&chat_id)
            .map(|items| items.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MessageId, UserId};

    fn message(chat_id: ChatId, id: i32, text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id,
            message_id: MessageId(id),
            from: Some(UserId(1)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            reply_to_bot: false,
        }
    }

    #[test]
    fn recent_for_chat_should_keep_only_the_newest_messages() {
        let mut buffer = RollingBuffer::new(2);

        buffer.push(message(ChatId(1), 1, "one"));
        buffer.push(message(ChatId(1), 2, "two"));
        buffer.push(message(ChatId(1), 3, "three"));

        let recent = buffer.recent_for_chat(ChatId(1));
        assert_eq!(recent[0].text, "two");
    }

    #[test]
    fn recent_for_chat_should_keep_messages_isolated_by_chat() {
        let mut buffer = RollingBuffer::new(3);

        buffer.push(message(ChatId(1), 1, "one"));
        buffer.push(message(ChatId(2), 2, "two"));

        let recent = buffer.recent_for_chat(ChatId(1));
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn new_should_treat_zero_capacity_as_one_message() {
        let mut buffer = RollingBuffer::new(0);

        buffer.push(message(ChatId(1), 1, "one"));
        buffer.push(message(ChatId(1), 2, "two"));

        let recent = buffer.recent_for_chat(ChatId(1));
        assert_eq!(recent[0].text, "two");
    }
}
