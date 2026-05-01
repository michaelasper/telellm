use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub i32);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SandboxId(String);

impl SandboxId {
    pub fn for_chat(chat_id: ChatId) -> Self {
        let raw = chat_id.0.to_string().replace('-', "neg");
        Self(format!("telellm-chat-{raw}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SandboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_id_for_chat_should_be_stable_for_positive_chat_ids() {
        let sandbox_id = SandboxId::for_chat(ChatId(12345));

        assert_eq!(sandbox_id.as_str(), "telellm-chat-12345");
    }

    #[test]
    fn sandbox_id_for_chat_should_not_include_minus_sign_for_negative_chat_ids() {
        let sandbox_id = SandboxId::for_chat(ChatId(-10012345));

        assert_eq!(sandbox_id.as_str(), "telellm-chat-neg10012345");
    }
}
