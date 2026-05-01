use crate::{
    bot::message::{AttachmentKind, IncomingAttachment},
    config::TelegramFormatMode,
    ids::{ChatId, MessageId},
};
use async_trait::async_trait;
use std::{path::Path, sync::Arc, time::Duration};
use teloxide::{
    dispatching::UpdateFilterExt,
    errors::AsResponseParameters,
    net::Download,
    payloads::{EditMessageTextSetters, SendMessageSetters},
    prelude::*,
    types::{
        BotCommand as TgBotCommand, ChatAction, ChatId as TgChatId, FileId, Message,
        MessageId as TgMessageId, ParseMode, User,
    },
    utils::{html, markdown},
};
use tokio::io::AsyncWriteExt;

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

    async fn download_file_to_path(
        &self,
        _file_id: &str,
        _destination: &Path,
    ) -> Result<u64, TelegramError> {
        Err(TelegramError::Download(
            "telegram file downloads are not supported by this sink".to_owned(),
        ))
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
    #[error("telegram file download failed: {0}")]
    Download(String),
    #[error("telegram command registration failed: {0}")]
    CommandRegistration(String),
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

    async fn download_file_to_path(
        &self,
        file_id: &str,
        destination: &Path,
    ) -> Result<u64, TelegramError> {
        let file = self
            .bot
            .get_file(FileId(file_id.to_owned()))
            .await
            .map_err(|err| TelegramError::Download(err.to_string()))?;
        let mut destination = tokio::fs::File::create(destination)
            .await
            .map_err(|err| TelegramError::Download(err.to_string()))?;

        self.bot
            .download_file(&file.path, &mut destination)
            .await
            .map_err(|err| TelegramError::Download(err.to_string()))?;
        destination
            .flush()
            .await
            .map_err(|err| TelegramError::Download(err.to_string()))?;
        let metadata = destination
            .metadata()
            .await
            .map_err(|err| TelegramError::Download(err.to_string()))?;

        Ok(metadata.len())
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
    register_bot_commands(&bot).await?;

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

async fn register_bot_commands(bot: &Bot) -> Result<(), TelegramError> {
    bot.set_my_commands(telegram_command_menu())
        .await
        .map(|_| ())
        .map_err(|err| TelegramError::CommandRegistration(err.to_string()))
}

fn telegram_command_menu() -> Vec<TgBotCommand> {
    crate::bot::command::command_definitions()
        .iter()
        .map(|definition| TgBotCommand::new(definition.command, definition.description))
        .collect()
}

fn normalize_message(
    message: &Message,
    bot_username: &str,
) -> Option<crate::bot::message::IncomingMessage> {
    let attachments = message_attachments(message);
    let text = message
        .text()
        .or_else(|| message.caption())
        .unwrap_or_default()
        .to_owned();
    if text.is_empty() && attachments.is_empty() {
        return None;
    }

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
        attachments,
        reply_to_bot,
        reply_to,
        private_chat: message.chat.is_private(),
    })
}

fn message_attachments(message: &Message) -> Vec<IncomingAttachment> {
    if let Some(photo) = message
        .photo()
        .and_then(|photos| photos.iter().max_by_key(|photo| photo.file.size))
    {
        return vec![IncomingAttachment::new(
            AttachmentKind::Photo,
            photo.file.id.0.clone(),
            photo.file.unique_id.0.clone(),
            Some("photo.jpg".to_owned()),
            Some("image/jpeg".to_owned()),
            u64::from(photo.file.size),
        )];
    }

    if let Some(document) = message.document() {
        return vec![IncomingAttachment::new(
            AttachmentKind::Document,
            document.file.id.0.clone(),
            document.file.unique_id.0.clone(),
            document.file_name.clone(),
            document.mime_type.as_ref().map(ToString::to_string),
            u64::from(document.file.size),
        )];
    }

    Vec::new()
}

fn normalize_replied_message(message: &Message) -> Option<crate::bot::message::RepliedMessage> {
    let attachments = message_attachments(message);
    let text = message
        .text()
        .or_else(|| message.caption())
        .unwrap_or_default()
        .to_owned();
    if text.is_empty() && attachments.is_empty() {
        return None;
    }

    Some(crate::bot::message::RepliedMessage {
        message_id: crate::ids::MessageId(message.id.0),
        from_name: message.from.as_ref().map(display_name),
        text,
        attachments,
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

    #[test]
    fn telegram_command_menu_should_include_remember() {
        let has_remember = telegram_command_menu()
            .into_iter()
            .any(|command| command.command == "remember");

        assert!(has_remember);
    }
}
