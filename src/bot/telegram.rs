use crate::ids::ChatId;
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use teloxide::{
    dispatching::UpdateFilterExt,
    errors::AsResponseParameters,
    prelude::*,
    types::{Message, User},
};

const RETRY_AFTER_BUFFER: Duration = Duration::from_millis(250);

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

    async fn send_chunk_with_retry(
        &self,
        chat_id: ChatId,
        chunk: &str,
    ) -> Result<(), TelegramError> {
        match self.send_chunk(chat_id, chunk).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let Some(retry_after) = retry_after_duration(&err) else {
                    return Err(telegram_send_error(err));
                };

                tokio::time::sleep(retry_after + RETRY_AFTER_BUFFER).await;
                self.send_chunk(chat_id, chunk)
                    .await
                    .map_err(telegram_send_error)
            }
        }
    }

    async fn send_chunk(&self, chat_id: ChatId, chunk: &str) -> Result<(), teloxide::RequestError> {
        self.bot
            .send_message(teloxide::types::ChatId(chat_id.0), chunk)
            .await
            .map(|_| ())
    }
}

#[async_trait]
impl TelegramSink for TeloxideTelegramSink {
    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
        for chunk in crate::bot::chunk::chunk_for_telegram(text, self.chunk_limit) {
            self.send_chunk_with_retry(chat_id, &chunk).await?;
        }
        Ok(())
    }
}

fn telegram_send_error(err: teloxide::RequestError) -> TelegramError {
    TelegramError::Send(err.to_string())
}

fn retry_after_duration(err: &teloxide::RequestError) -> Option<Duration> {
    err.retry_after()
        .map(|seconds| seconds.duration())
        .or_else(|| parse_retry_after_duration(&err.to_string()))
}

fn parse_retry_after_duration(text: &str) -> Option<Duration> {
    // Structured teloxide retry-after errors are preferred. This fallback only
    // handles display strings containing the narrow phrase "retry after <seconds>".
    let lower = text.to_ascii_lowercase();
    let (_, tail) = lower.split_once("retry after")?;
    let digits: String = tail
        .trim_start_matches(|ch: char| !ch.is_ascii_digit())
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();

    let seconds = digits.parse::<u64>().ok()?;
    Some(Duration::from_secs(seconds))
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
        private_chat: message.chat.is_private(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn retry_after_parser_should_extract_seconds() {
        let error = "Telegram error: Too Many Requests: retry after 7";

        assert_eq!(
            parse_retry_after_duration(error),
            Some(Duration::from_secs(7))
        );
    }
}
