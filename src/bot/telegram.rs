use crate::ids::ChatId;
use async_trait::async_trait;

#[async_trait]
pub trait TelegramSink: Send + Sync {
    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), TelegramError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("telegram send failed: {0}")]
    Send(String),
}

pub struct TelegramAdapter;
