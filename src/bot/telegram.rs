use crate::{
    config::TelegramFormatMode,
    ids::{ChatId, MessageId},
};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use teloxide::{
    dispatching::UpdateFilterExt,
    errors::AsResponseParameters,
    payloads::{EditMessageTextSetters, SendMessageSetters},
    prelude::*,
    types::{ChatAction, ChatId as TgChatId, Message, MessageId as TgMessageId, ParseMode, User},
    utils::{html, markdown},
};

const RETRY_AFTER_BUFFER: Duration = Duration::from_millis(250);

#[async_trait]
pub trait TelegramSink: Send + Sync {
    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), TelegramError>;

    async fn send_message_with_options(
        &self,
        chat_id: ChatId,
        text: &str,
        _options: TelegramSendOptions,
    ) -> Result<Vec<TelegramMessageHandle>, TelegramError> {
        self.send_message(chat_id, text).await?;
        Ok(Vec::new())
    }

    async fn edit_message_with_options(
        &self,
        chat_id: ChatId,
        _message_id: MessageId,
        text: &str,
        options: TelegramSendOptions,
    ) -> Result<(), TelegramError> {
        self.send_message_with_options(chat_id, text, options)
            .await?;
        Ok(())
    }

    async fn finish_streamed_message(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
        text: &str,
        options: TelegramSendOptions,
    ) -> Result<(), TelegramError> {
        self.edit_message_with_options(chat_id, message_id, text, options)
            .await
    }

    async fn send_typing_action(&self, _chat_id: ChatId) -> Result<(), TelegramError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelegramMessageHandle {
    pub chat_id: ChatId,
    pub message_id: MessageId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelegramSendOptions {
    pub formatting_mode: TelegramFormatMode,
    pub formatting_escape: bool,
    pub formatting_fallback_to_plain: bool,
}

impl TelegramSendOptions {
    pub fn plain() -> Self {
        Self {
            formatting_mode: TelegramFormatMode::Plain,
            formatting_escape: false,
            formatting_fallback_to_plain: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("telegram send failed: {0}")]
    Send(String),
    #[error("telegram edit failed: {0}")]
    Edit(String),
    #[error("telegram chat action failed: {0}")]
    ChatAction(String),
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
        options: TelegramSendOptions,
    ) -> Result<Message, TelegramError> {
        match self.send_chunk(chat_id, chunk, options).await {
            Ok(message) => Ok(message),
            Err(err) => {
                let Some(retry_after) = retry_after_duration(&err) else {
                    return Err(telegram_send_error(err));
                };

                tokio::time::sleep(retry_after + RETRY_AFTER_BUFFER).await;
                self.send_chunk(chat_id, chunk, options)
                    .await
                    .map_err(telegram_send_error)
            }
        }
    }

    async fn send_chunk_with_fallback(
        &self,
        chat_id: ChatId,
        chunk: &str,
        options: TelegramSendOptions,
    ) -> Result<Message, TelegramError> {
        match self.send_chunk_with_retry(chat_id, chunk, options).await {
            Ok(message) => Ok(message),
            Err(_)
                if options.formatting_fallback_to_plain
                    && options.formatting_mode != TelegramFormatMode::Plain =>
            {
                self.send_chunk_with_retry(chat_id, chunk, TelegramSendOptions::plain())
                    .await
            }
            Err(err) => Err(err),
        }
    }

    async fn send_chunk(
        &self,
        chat_id: ChatId,
        chunk: &str,
        options: TelegramSendOptions,
    ) -> Result<Message, teloxide::RequestError> {
        let (text, parse_mode) = render_for_telegram(chunk, options);
        let request = self.bot.send_message(TgChatId(chat_id.0), text);
        match parse_mode {
            Some(parse_mode) => request.parse_mode(parse_mode).await,
            None => request.await,
        }
    }

    async fn edit_chunk_with_retry(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
        chunk: &str,
        options: TelegramSendOptions,
    ) -> Result<Message, TelegramError> {
        match self.edit_chunk(chat_id, message_id, chunk, options).await {
            Ok(message) => Ok(message),
            Err(err) => {
                let Some(retry_after) = retry_after_duration(&err) else {
                    return Err(telegram_edit_error(err));
                };

                tokio::time::sleep(retry_after + RETRY_AFTER_BUFFER).await;
                self.edit_chunk(chat_id, message_id, chunk, options)
                    .await
                    .map_err(telegram_edit_error)
            }
        }
    }

    async fn edit_chunk_with_fallback(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
        chunk: &str,
        options: TelegramSendOptions,
    ) -> Result<Message, TelegramError> {
        match self
            .edit_chunk_with_retry(chat_id, message_id, chunk, options)
            .await
        {
            Ok(message) => Ok(message),
            Err(_)
                if options.formatting_fallback_to_plain
                    && options.formatting_mode != TelegramFormatMode::Plain =>
            {
                self.edit_chunk_with_retry(chat_id, message_id, chunk, TelegramSendOptions::plain())
                    .await
            }
            Err(err) => Err(err),
        }
    }

    async fn edit_chunk(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
        chunk: &str,
        options: TelegramSendOptions,
    ) -> Result<Message, teloxide::RequestError> {
        let (text, parse_mode) = render_for_telegram(chunk, options);
        let request =
            self.bot
                .edit_message_text(TgChatId(chat_id.0), TgMessageId(message_id.0), text);
        match parse_mode {
            Some(parse_mode) => request.parse_mode(parse_mode).await,
            None => request.await,
        }
    }
}

#[async_trait]
impl TelegramSink for TeloxideTelegramSink {
    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
        self.send_message_with_options(chat_id, text, TelegramSendOptions::plain())
            .await?;
        Ok(())
    }

    async fn send_message_with_options(
        &self,
        chat_id: ChatId,
        text: &str,
        options: TelegramSendOptions,
    ) -> Result<Vec<TelegramMessageHandle>, TelegramError> {
        let mut handles = Vec::new();
        for chunk in crate::bot::chunk::chunk_for_telegram(text, self.chunk_limit) {
            let message = self
                .send_chunk_with_fallback(chat_id, &chunk, options)
                .await?;
            handles.push(TelegramMessageHandle {
                chat_id,
                message_id: MessageId(message.id.0),
            });
        }
        Ok(handles)
    }

    async fn edit_message_with_options(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
        text: &str,
        options: TelegramSendOptions,
    ) -> Result<(), TelegramError> {
        let preview = streaming_preview(text, self.chunk_limit);
        self.edit_chunk_with_fallback(chat_id, message_id, &preview, options)
            .await?;
        Ok(())
    }

    async fn finish_streamed_message(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
        text: &str,
        options: TelegramSendOptions,
    ) -> Result<(), TelegramError> {
        let chunks = crate::bot::chunk::chunk_for_telegram(text, self.chunk_limit);
        let Some((first, rest)) = chunks.split_first() else {
            return Ok(());
        };

        if let Err(err) = self
            .edit_chunk_with_fallback(chat_id, message_id, first, options)
            .await
        {
            tracing::warn!(
                error = %err,
                chat_id = ?chat_id,
                message_id = ?message_id,
                "failed to edit streamed Telegram message with final Codex response; sending a new message"
            );
            self.send_message_with_options(chat_id, text, options)
                .await?;
            return Ok(());
        }

        for chunk in rest {
            self.send_chunk_with_fallback(chat_id, chunk, options)
                .await?;
        }
        Ok(())
    }

    async fn send_typing_action(&self, chat_id: ChatId) -> Result<(), TelegramError> {
        self.bot
            .send_chat_action(TgChatId(chat_id.0), ChatAction::Typing)
            .await
            .map(|_| ())
            .map_err(|err| TelegramError::ChatAction(err.to_string()))
    }
}

fn telegram_send_error(err: teloxide::RequestError) -> TelegramError {
    TelegramError::Send(err.to_string())
}

fn telegram_edit_error(err: teloxide::RequestError) -> TelegramError {
    TelegramError::Edit(err.to_string())
}

fn render_for_telegram(text: &str, options: TelegramSendOptions) -> (String, Option<ParseMode>) {
    match options.formatting_mode {
        TelegramFormatMode::Plain => (text.to_owned(), None),
        TelegramFormatMode::MarkdownV2 => {
            let rendered = if options.formatting_escape {
                markdown::escape(text)
            } else {
                text.to_owned()
            };
            (rendered, Some(ParseMode::MarkdownV2))
        }
        TelegramFormatMode::Html => {
            let rendered = if options.formatting_escape {
                html::escape(text)
            } else {
                text.to_owned()
            };
            (rendered, Some(ParseMode::Html))
        }
    }
}

fn streaming_preview(text: &str, limit: usize) -> String {
    crate::bot::chunk::chunk_for_telegram(text, limit)
        .into_iter()
        .next()
        .unwrap_or_default()
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
    let replied_message = message.reply_to_message();
    let reply_to_bot = replied_message
        .and_then(|reply| reply.from.as_ref())
        .and_then(|user| user.username.as_deref())
        .is_some_and(|username| username.eq_ignore_ascii_case(bot_username));
    let reply_to = replied_message.and_then(normalize_replied_message);

    Some(crate::bot::message::IncomingMessage {
        chat_id: ChatId(message.chat.id.0),
        message_id: crate::ids::MessageId(message.id.0),
        from,
        from_name,
        text,
        reply_to_bot,
        reply_to,
        private_chat: message.chat.is_private(),
    })
}

fn normalize_replied_message(message: &Message) -> Option<crate::bot::message::RepliedMessage> {
    Some(crate::bot::message::RepliedMessage {
        message_id: crate::ids::MessageId(message.id.0),
        from_name: message.from.as_ref().map(display_name),
        text: message.text()?.to_owned(),
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

    #[test]
    fn render_for_telegram_should_escape_markdown_v2_when_configured() {
        let options = TelegramSendOptions {
            formatting_mode: TelegramFormatMode::MarkdownV2,
            formatting_escape: true,
            formatting_fallback_to_plain: true,
        };

        let (text, parse_mode) = render_for_telegram("hi *there*", options);

        assert_eq!(text, "hi \\*there\\*");
        assert_eq!(parse_mode, Some(ParseMode::MarkdownV2));
    }

    #[test]
    fn render_for_telegram_should_leave_plain_text_unparsed() {
        let (text, parse_mode) = render_for_telegram("hi *there*", TelegramSendOptions::plain());

        assert_eq!(text, "hi *there*");
        assert_eq!(parse_mode, None);
    }
}
