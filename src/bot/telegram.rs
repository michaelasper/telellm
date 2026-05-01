use crate::ids::ChatId;
use async_trait::async_trait;
use std::sync::Arc;
use teloxide::{
    dispatching::UpdateFilterExt,
    prelude::*,
    types::{Message, User},
};

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

pub struct TeloxideTelegramSink {
    bot: Bot,
    chunk_limit: usize,
}

impl TeloxideTelegramSink {
    pub fn new(bot: Bot, chunk_limit: usize) -> Self {
        Self { bot, chunk_limit }
    }
}

#[async_trait]
impl TelegramSink for TeloxideTelegramSink {
    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
        for chunk in crate::bot::chunk::chunk_for_telegram(text, self.chunk_limit) {
            self.bot
                .send_message(teloxide::types::ChatId(chat_id.0), chunk)
                .await
                .map_err(|err| TelegramError::Send(err.to_string()))?;
        }
        Ok(())
    }
}

#[async_trait]
pub trait IncomingMessageHandler: Send + Sync {
    async fn handle_message(&self, message: crate::bot::message::IncomingMessage);
}

pub async fn run_polling(
    bot: Bot,
    bot_username: String,
    handler: Arc<dyn IncomingMessageHandler>,
) -> Result<(), TelegramError> {
    let handler_filter = Update::filter_message().endpoint(move |msg: Message| {
        let handler = handler.clone();
        let bot_username = bot_username.clone();
        async move {
            if let Some(incoming) = normalize_message(&msg, &bot_username) {
                handler.handle_message(incoming).await;
            }
            respond(())
        }
    });

    Dispatcher::builder(bot, handler_filter)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}

fn normalize_message(
    message: &Message,
    bot_username: &str,
) -> Option<crate::bot::message::IncomingMessage> {
    let text = message.text()?.to_owned();
    let from = message.from.as_ref().map(user_id);
    let from_name = message.from.as_ref().map(display_name);
    let reply_to_bot = message
        .reply_to_message()
        .and_then(|reply| reply.from.as_ref())
        .and_then(|user| user.username.as_deref())
        .is_some_and(|username| username.eq_ignore_ascii_case(bot_username));

    Some(crate::bot::message::IncomingMessage {
        chat_id: ChatId(message.chat.id.0),
        message_id: crate::ids::MessageId(message.id.0),
        from,
        from_name,
        text,
        reply_to_bot,
    })
}

fn user_id(user: &User) -> crate::ids::UserId {
    crate::ids::UserId(user.id.0)
}

fn display_name(user: &User) -> String {
    match &user.last_name {
        Some(last_name) => format!("{} {}", user.first_name, last_name),
        None => user.first_name.clone(),
    }
}
