use crate::{
    bot::{
        command::{BotCommand, ForgetTarget},
        message::{Addressing, IncomingAttachment, IncomingMessage},
        telegram::{IncomingMessageHandler, TelegramError},
    },
    config::{AppConfig, AttachmentConfig, CodexAuthMode, OutputConfig},
    memory::{MemoryKind, MemoryStore, RollingBuffer, context::ContextPacket},
    router::{GroupWorkItem, Router, RouterError},
    runtime::{ChatRuntimeStatus, RuntimeControl, RuntimeState},
};
use anyhow::Context;
use async_trait::async_trait;
use secrecy::ExposeSecret;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

pub async fn run(config: AppConfig) -> anyhow::Result<()> {
    if let Some(parent) = config
        .storage
        .sqlite_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create data directory {}", parent.display()))?;
    }

    if config.codex.auth_mode == CodexAuthMode::BrokerApiKey {
        let broker = config.broker_config()?;
        let upstream_api_key = std::env::var(&broker.upstream_api_key_env).with_context(|| {
            format!(
                "required upstream API key env var `{}` is not set",
                broker.upstream_api_key_env
            )
        })?;
        let broker_state = crate::broker::BrokerState::new_with_limits(
            crate::broker::BrokerConfig {
                listen: broker.listen,
                upstream_base_url: broker.upstream_base_url.clone(),
                upstream_api_key: secrecy::SecretString::from(upstream_api_key),
            },
            crate::config::broker_limits(&config),
        );
        let broker_listener = tokio::net::TcpListener::bind(broker.listen)
            .await
            .with_context(|| format!("failed to bind broker at {}", broker.listen))?;
        tokio::spawn(async move {
            if let Err(err) =
                axum::serve(broker_listener, crate::broker::router(broker_state)).await
            {
                tracing::error!(error = %err, "broker server stopped");
            }
        });
    }

    let telegram_token = config.telegram_token_from_env()?;
    let bot = teloxide::Bot::new(telegram_token.expose_secret().to_owned());
    let telegram_sink = Arc::new(crate::bot::telegram::TeloxideTelegramSink::new(
        bot.clone(),
        config.limits.telegram_chunk_chars,
    ));
    let allowed_chat_ids = config.telegram.allowed_chat_ids.clone();
    let router = Arc::new(crate::router::Router::new_with_telegram_ux(
        config.limits.per_group_queue_depth,
        telegram_sink,
        config.telegram_ux.clone(),
    ));
    let memory_url = format!("sqlite://{}?mode=rwc", config.storage.sqlite_path.display());
    let memory_store = Arc::new(crate::memory::SqliteMemoryStore::connect(&memory_url).await?);
    let runtime = Arc::new(crate::runtime::RuntimeManager::new(
        crate::sandbox::docker::DockerSandboxBackend,
        config.docker.clone(),
        config.codex.clone(),
        config.outputs.clone(),
        config.broker.clone(),
        router.clone(),
        crate::config::codex_read_policy(&config),
    ));
    let app_core_config = AppCoreConfig {
        bot_username: config.telegram.bot_username.clone(),
        system_prompt: config.prompt.system_prompt.clone(),
        allowed_chat_ids,
        queue_ack_enabled: config.telegram_ux.queue_ack_enabled,
        attachments: config.attachments.clone(),
        outputs: config.outputs.clone(),
    };
    let app_core = Arc::new(AppCore::new(
        app_core_config,
        memory_store,
        RollingBuffer::new(config.limits.recent_buffer_messages),
        router,
        runtime,
    ));

    crate::bot::telegram::run_polling(bot, config.telegram.bot_username, app_core).await?;
    Ok(())
}

pub struct AppCore<M, S, T, N>
where
    M: MemoryStore + 'static,
    S: crate::codex::session::CodexSession + 'static,
    T: crate::bot::telegram::TelegramSink + 'static,
    N: RuntimeControl + 'static,
{
    bot_username: String,
    system_prompt: String,
    allowed_chat_ids: Vec<crate::ids::ChatId>,
    queue_ack_enabled: bool,
    attachments: AttachmentConfig,
    outputs: OutputConfig,
    memory_store: Arc<M>,
    rolling: Arc<Mutex<RollingBuffer>>,
    router: Arc<Router<S, T>>,
    runtime: Arc<N>,
}

#[derive(Debug, Clone)]
pub struct AppCoreConfig {
    pub bot_username: String,
    pub system_prompt: String,
    pub allowed_chat_ids: Vec<crate::ids::ChatId>,
    pub queue_ack_enabled: bool,
    pub attachments: AttachmentConfig,
    pub outputs: OutputConfig,
}

impl<M, S, T, N> AppCore<M, S, T, N>
where
    M: MemoryStore + 'static,
    S: crate::codex::session::CodexSession + 'static,
    T: crate::bot::telegram::TelegramSink + 'static,
    N: RuntimeControl + 'static,
{
    pub fn new(
        config: AppCoreConfig,
        memory_store: Arc<M>,
        rolling: RollingBuffer,
        router: Arc<Router<S, T>>,
        runtime: Arc<N>,
    ) -> Self {
        let AppCoreConfig {
            bot_username,
            system_prompt,
            allowed_chat_ids,
            queue_ack_enabled,
            attachments,
            outputs,
        } = config;

        Self {
            bot_username,
            system_prompt,
            allowed_chat_ids,
            queue_ack_enabled,
            attachments,
            outputs,
            memory_store,
            rolling: Arc::new(Mutex::new(rolling)),
            router,
            runtime,
        }
    }

    pub async fn handle_message(&self, mut message: IncomingMessage) -> Result<(), AppError> {
        let command = BotCommand::parse(&message.text, &self.bot_username)?;

        if !self.chat_allowed(message.chat_id) {
            return Ok(());
        }

        if command.is_none() && message.addressing(&self.bot_username) == Addressing::Ambient {
            self.rolling.lock().await.push(message);
            return Ok(());
        }

        if let Some(command) = command {
            self.rolling.lock().await.push(message.clone());
            return self.handle_command(message, command).await;
        }

        self.runtime.ensure_chat_runtime(message.chat_id).await?;
        self.prepare_attachments(&mut message).await;
        self.rolling.lock().await.push(message.clone());

        let recent_messages = self.rolling.lock().await.recent_for_chat(message.chat_id);
        let memories = self.memory_store.list_memories(message.chat_id).await?;
        let packet = ContextPacket {
            system_prompt: self.system_prompt_for_turn(),
            triggering_message: message.clone(),
            recent_messages,
            memories,
        };
        self.enqueue_text(message.chat_id, packet.render()).await
    }

    async fn handle_command(
        &self,
        message: IncomingMessage,
        command: BotCommand,
    ) -> Result<(), AppError> {
        match command {
            BotCommand::Help => {
                self.reply_text(message.chat_id, help_text()).await?;
            }
            BotCommand::Status => {
                let status = self.runtime.chat_status(message.chat_id).await;
                self.reply_text(message.chat_id, status_text(status))
                    .await?;
            }
            BotCommand::Reset { clear_workspace } => {
                self.runtime
                    .reset_chat_runtime(message.chat_id, clear_workspace)
                    .await?;
                self.reply_text(message.chat_id, "The group Codex session has been reset.")
                    .await?;
            }
            BotCommand::Restart => {
                self.runtime.restart_chat_runtime(message.chat_id).await?;
                self.reply_text(message.chat_id, "The group sandbox has been restarted.")
                    .await?;
            }
            BotCommand::Rebuild { clear_workspace } => {
                self.runtime
                    .rebuild_chat_runtime(message.chat_id, clear_workspace)
                    .await?;
                self.reply_text(message.chat_id, "The group sandbox has been rebuilt.")
                    .await?;
            }
            BotCommand::Memory => {
                let memories = self.memory_store.list_memories(message.chat_id).await?;
                self.reply_text(
                    message.chat_id,
                    format!("Durable group memories: {memories:?}"),
                )
                .await?;
            }
            BotCommand::Remember { content } => {
                self.memory_store
                    .add_memory(
                        message.chat_id,
                        message.from,
                        MemoryKind::Personality,
                        &content,
                    )
                    .await?;
                self.reply_text(message.chat_id, "Remembered that for this group.")
                    .await?;
            }
            BotCommand::Forget { target } => match target {
                ForgetTarget::All => {
                    let removed = self.memory_store.forget_all(message.chat_id).await?;
                    self.reply_text(
                        message.chat_id,
                        format!("Forgot {removed} durable memory records for this group."),
                    )
                    .await?;
                }
                ForgetTarget::Query(query) => {
                    self.reply_text(
                        message.chat_id,
                        format!(
                            "Targeted forget was requested for `{query}`. Ask for `/forget all` to clear durable group memory."
                        ),
                    )
                    .await?;
                }
            },
        }
        Ok(())
    }

    fn chat_allowed(&self, chat_id: crate::ids::ChatId) -> bool {
        self.allowed_chat_ids.is_empty() || self.allowed_chat_ids.contains(&chat_id)
    }

    fn system_prompt_for_turn(&self) -> String {
        let Some(instruction) = crate::output::prompt_instruction(&self.outputs) else {
            return self.system_prompt.clone();
        };

        format!("{}\n\n{}", self.system_prompt.trim(), instruction)
    }

    async fn reply_text(
        &self,
        chat_id: crate::ids::ChatId,
        text: impl AsRef<str>,
    ) -> Result<(), AppError> {
        self.router
            .telegram()
            .send_message(chat_id, text.as_ref())
            .await?;
        Ok(())
    }

    async fn prepare_attachments(&self, message: &mut IncomingMessage) {
        self.prepare_attachment_list(
            message.chat_id,
            message.message_id,
            &mut message.attachments,
        )
        .await;
        if let Some(reply_to) = &mut message.reply_to {
            self.prepare_attachment_list(
                message.chat_id,
                reply_to.message_id,
                &mut reply_to.attachments,
            )
            .await;
        }
    }

    async fn prepare_attachment_list(
        &self,
        chat_id: crate::ids::ChatId,
        message_id: crate::ids::MessageId,
        attachments: &mut [IncomingAttachment],
    ) {
        if attachments.is_empty() {
            return;
        }

        if !self.attachments.enabled {
            for attachment in attachments {
                attachment.skipped_reason =
                    Some("attachment downloads are disabled by configuration".to_owned());
            }
            return;
        }

        for (index, attachment) in attachments.iter_mut().enumerate() {
            match self
                .import_attachment(chat_id, message_id, index, attachment)
                .await
            {
                Ok(workspace_path) => attachment.workspace_path = Some(workspace_path),
                Err(reason) => {
                    tracing::warn!(
                        chat_id = ?chat_id,
                        message_id = ?message_id,
                        file_unique_id = %attachment.file_unique_id,
                        reason = %reason,
                        "failed to import Telegram attachment"
                    );
                    attachment.skipped_reason = Some(reason);
                }
            }
        }
    }

    async fn import_attachment(
        &self,
        chat_id: crate::ids::ChatId,
        message_id: crate::ids::MessageId,
        index: usize,
        attachment: &IncomingAttachment,
    ) -> Result<String, String> {
        if attachment.file_size > self.attachments.max_file_bytes {
            return Err(format!(
                "file size {} exceeds configured limit {} bytes",
                attachment.file_size, self.attachments.max_file_bytes
            ));
        }

        let workspace_path = attachment_workspace_path(
            &self.attachments.workspace_dir,
            message_id,
            index,
            attachment,
        );
        let temp_path = temp_attachment_path(message_id, index);
        let result = async {
            let downloaded_bytes = self
                .router
                .telegram()
                .download_file_to_path(&attachment.file_id, &temp_path)
                .await
                .map_err(|err| err.to_string())?;
            if downloaded_bytes > self.attachments.max_file_bytes {
                return Err(format!(
                    "downloaded file size {downloaded_bytes} exceeds configured limit {} bytes",
                    self.attachments.max_file_bytes
                ));
            }
            self.runtime
                .import_chat_attachment(chat_id, &temp_path, &workspace_path)
                .await
                .map_err(|err| err.to_string())?;
            Ok(workspace_path)
        }
        .await;

        if let Err(err) = tokio::fs::remove_file(&temp_path).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                error = %err,
                path = %temp_path.display(),
                "failed to remove temporary Telegram attachment"
            );
        }

        result
    }

    async fn enqueue_text(
        &self,
        chat_id: crate::ids::ChatId,
        prompt: impl Into<String>,
    ) -> Result<(), AppError> {
        let receipt = self
            .router
            .enqueue(GroupWorkItem {
                chat_id,
                prompt: prompt.into(),
            })
            .await?;
        if self.queue_ack_enabled && receipt.queue_position > 0 {
            self.reply_text(
                chat_id,
                format!(
                    "Queued behind {} existing request(s).",
                    receipt.queue_position
                ),
            )
            .await?;
        }
        Ok(())
    }
}

fn attachment_workspace_path(
    workspace_dir: &str,
    message_id: crate::ids::MessageId,
    index: usize,
    attachment: &IncomingAttachment,
) -> String {
    let workspace_dir = workspace_dir.trim_matches('/');
    let file_name = attachment
        .file_name
        .as_deref()
        .unwrap_or_else(|| attachment.kind.default_file_name());
    format!(
        "{workspace_dir}/msg-{}/{}-{}",
        message_id.0,
        index + 1,
        sanitize_file_name(file_name)
    )
}

fn sanitize_file_name(file_name: &str) -> String {
    let mut sanitized = String::with_capacity(file_name.len().min(128));
    for ch in file_name.chars().take(128) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }

    let sanitized = sanitized.trim_matches('_');
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return "attachment".to_owned();
    }
    if sanitized.starts_with('.') {
        return format!("attachment{sanitized}");
    }
    sanitized.to_owned()
}

fn temp_attachment_path(message_id: crate::ids::MessageId, index: usize) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "telellm-upload-{}-{}-{index}-{nanos}",
        std::process::id(),
        message_id.0
    ))
}

#[async_trait]
impl<M, S, T, N> IncomingMessageHandler for AppCore<M, S, T, N>
where
    M: MemoryStore + 'static,
    S: crate::codex::session::CodexSession + 'static,
    T: crate::bot::telegram::TelegramSink + 'static,
    N: RuntimeControl + 'static,
{
    async fn handle_message(&self, message: IncomingMessage) {
        let chat_id = message.chat_id;
        if let Err(err) = AppCore::handle_message(self, message).await {
            tracing::warn!(error = %err, "failed to handle Telegram message");
            if let Some(text) = user_visible_error(&err)
                && let Err(send_err) = self.router.telegram().send_message(chat_id, text).await
            {
                tracing::warn!(
                    error = %send_err,
                    chat_id = ?chat_id,
                    "failed to send app error to Telegram"
                );
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("command parse error: {0}")]
    Command(#[from] crate::bot::command::CommandParseError),
    #[error("memory error: {0}")]
    Memory(#[from] crate::memory::MemoryStoreError),
    #[error("router error: {0}")]
    Router(#[from] RouterError),
    #[error("runtime error: {0}")]
    Runtime(#[from] crate::runtime::RuntimeError),
    #[error("telegram error: {0}")]
    Telegram(#[from] TelegramError),
}

fn help_text() -> &'static str {
    "Show concise help for /status /reset /restart /rebuild /memory /remember /forget."
}

fn user_visible_error(error: &AppError) -> Option<&'static str> {
    match error {
        AppError::Runtime(_) => {
            Some("I could not start this group's Codex runtime. Check the daemon logs for details.")
        }
        AppError::Router(_) => {
            Some("I could not queue that request for Codex. Check the daemon logs for details.")
        }
        AppError::Command(_) | AppError::Memory(_) | AppError::Telegram(_) => None,
    }
}

fn status_text(status: ChatRuntimeStatus) -> String {
    match status.state {
        RuntimeState::NotStarted => {
            format!("Status: not started. Generation: {}.", status.generation)
        }
        RuntimeState::Starting => {
            format!("Status: starting. Generation: {}.", status.generation)
        }
        RuntimeState::Ready => {
            format!("Status: ready. Generation: {}.", status.generation)
        }
        RuntimeState::Degraded(reason) => {
            format!(
                "Status: degraded. Generation: {}. Reason: {reason}",
                status.generation
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bot::message::AttachmentKind,
        bot::telegram::{TelegramError, TelegramSink},
        codex::session::{CodexRequest, CodexSession, CodexSessionError, CodexTurn},
        ids::{ChatId, MessageId, UserId},
        memory::{MemoryKind, MemoryRecord, MemoryStoreError},
    };
    use async_trait::async_trait;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        sync::mpsc,
        time::{Duration, timeout},
    };

    fn incoming(text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id: ChatId(1),
            message_id: MessageId(1),
            from: Some(UserId(2)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            attachments: Vec::new(),
            reply_to_bot: false,
            reply_to: None,
            private_chat: false,
        }
    }

    fn test_system_prompt() -> String {
        "Test system prompt.".to_owned()
    }

    fn app_core_config(allowed_chat_ids: Vec<ChatId>, queue_ack_enabled: bool) -> AppCoreConfig {
        AppCoreConfig {
            bot_username: "telellm_bot".to_owned(),
            system_prompt: test_system_prompt(),
            allowed_chat_ids,
            queue_ack_enabled,
            attachments: AttachmentConfig::default(),
            outputs: OutputConfig::default(),
        }
    }

    #[tokio::test]
    async fn handle_message_should_ignore_ambient_messages_without_runtime() {
        let (app, runtime, _messages) = app_with_fakes().await;

        app.handle_message(incoming("plain group chat"))
            .await
            .expect("ambient message should be accepted");

        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn handle_message_should_ignore_disallowed_chat_without_runtime() {
        let (app, runtime, _messages) = app_with_fakes_and_allowed_chats(vec![ChatId(999)]).await;

        app.handle_message(incoming("@telellm_bot hello"))
            .await
            .expect("disallowed chat should be ignored");

        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn handle_message_should_enqueue_addressed_context() {
        let (app, runtime, mut messages) = app_with_fakes().await;

        app.handle_message(incoming("@telellm_bot hello"))
            .await
            .expect("addressed message should be handled");
        let response = messages.recv().await.expect("response should be sent");

        assert!(response.starts_with("Test system prompt."));
        assert!(response.contains("@telellm_bot hello"));
        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn handle_message_should_enqueue_plain_private_chat_context() {
        let (app, runtime, mut messages) = app_with_fakes().await;
        let mut message = incoming("hello from a DM");
        message.private_chat = true;

        app.handle_message(message)
            .await
            .expect("plain private chat message should be handled");
        let response = messages.recv().await.expect("response should be sent");

        assert!(response.contains("hello from a DM"));
        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn handle_message_should_clear_memory_for_forget_all() {
        let (app, runtime, mut messages) = app_with_fakes().await;

        app.handle_message(incoming("/forget all"))
            .await
            .expect("forget command should be handled");
        let response = messages.recv().await.expect("response should be sent");

        assert!(response.contains("Forgot 1 durable memory records"));
        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn help_should_reply_without_runtime() {
        let (app, runtime, mut messages) = app_with_fakes().await;

        app.handle_message(incoming("/help"))
            .await
            .expect("help should reply");

        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 0);
        assert!(messages.recv().await.expect("reply").contains("/status"));
    }

    #[tokio::test]
    async fn forget_all_should_not_start_codex() {
        let (app, runtime, mut messages) = app_with_fakes().await;

        app.handle_message(incoming("/forget all"))
            .await
            .expect("forget should reply");

        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 0);
        assert!(messages.recv().await.expect("reply").contains("Forgot"));
    }

    #[tokio::test]
    async fn remember_should_write_memory_without_runtime() {
        let (app, runtime, mut messages) = app_with_fakes().await;

        app.handle_message(incoming("/remember Mike likes short answers"))
            .await
            .expect("remember should work");

        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 0);
        assert_eq!(app.memory_store.remembered.load(Ordering::SeqCst), 1);
        assert!(messages.recv().await.expect("reply").contains("Remembered"));
    }

    #[tokio::test]
    async fn status_should_report_degraded_runtime_without_codex_prompt() {
        let (app, runtime, mut messages, codex) = app_with_fakes_and_session().await;
        runtime
            .set_status(ChatRuntimeStatus {
                chat_id: ChatId(1),
                state: RuntimeState::Degraded("docker unavailable".to_owned()),
                generation: 7,
            })
            .await;

        app.handle_message(incoming("/status"))
            .await
            .expect("status should reply");
        let reply = messages.recv().await.expect("reply should be sent");

        assert!(reply.contains("degraded"));
        assert!(reply.contains("docker unavailable"));
        assert_eq!(codex.prompts.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn addressed_message_should_acknowledge_long_running_turn() {
        let memory = Arc::new(FakeMemoryStore::default());
        let (telegram, mut messages) = FakeTelegram::new();
        let router = Arc::new(Router::new(4, telegram));
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
        let codex = Arc::new(BlockingCodexSession::new(entered_tx));
        router.register_session(ChatId(1), 1, codex.clone()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let app = AppCore::new(
            app_core_config(Vec::new(), true),
            memory,
            RollingBuffer::new(10),
            router,
            runtime,
        );

        app.handle_message(incoming("@telellm_bot first"))
            .await
            .expect("first addressed message should be handled");
        timeout(Duration::from_secs(1), entered_rx.recv())
            .await
            .expect("first Codex turn should start")
            .expect("blocking session should report the first prompt");

        app.handle_message(incoming("@telellm_bot second"))
            .await
            .expect("second addressed message should be handled");
        let ack = timeout(Duration::from_secs(1), messages.recv())
            .await
            .expect("queued acknowledgement should arrive")
            .expect("telegram sender should stay open");

        assert_eq!(ack, "Queued behind 1 existing request(s).");
    }

    #[tokio::test]
    async fn addressed_message_should_skip_queue_acknowledgement_when_disabled() {
        let memory = Arc::new(FakeMemoryStore::default());
        let (telegram, mut messages) = FakeTelegram::new();
        let router = Arc::new(Router::new(4, telegram));
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
        let codex = Arc::new(BlockingCodexSession::new(entered_tx));
        router.register_session(ChatId(1), 1, codex.clone()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let app = AppCore::new(
            app_core_config(Vec::new(), false),
            memory,
            RollingBuffer::new(10),
            router,
            runtime,
        );

        app.handle_message(incoming("@telellm_bot first"))
            .await
            .expect("first addressed message should be handled");
        timeout(Duration::from_secs(1), entered_rx.recv())
            .await
            .expect("first Codex turn should start")
            .expect("blocking session should report the first prompt");

        app.handle_message(incoming("@telellm_bot second"))
            .await
            .expect("second addressed message should be handled");

        assert!(matches!(
            messages.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn runtime_start_failure_should_send_telegram_error() {
        let (app, _runtime, mut messages) = app_with_failing_runtime().await;

        IncomingMessageHandler::handle_message(&app, incoming("@telellm_bot hello")).await;

        assert!(
            messages
                .recv()
                .await
                .expect("reply")
                .contains("could not start")
        );
    }

    #[tokio::test]
    async fn addressed_message_should_import_attachment_and_render_workspace_path() {
        let (app, runtime, mut messages) = app_with_fakes().await;
        let mut message = incoming("@telellm_bot describe this");
        message.attachments.push(IncomingAttachment::new(
            AttachmentKind::Photo,
            "telegram-file-id".to_owned(),
            "telegram-unique-id".to_owned(),
            Some("cat.jpg".to_owned()),
            Some("image/jpeg".to_owned()),
            15,
        ));

        app.handle_message(message)
            .await
            .expect("addressed message should be handled");
        let response = messages.recv().await.expect("response should be sent");
        let imported = runtime.imported.lock().await;

        assert_eq!(imported.len(), 1);
        assert_eq!(
            imported[0].workspace_path,
            "telegram_uploads/msg-1/1-cat.jpg"
        );
        assert_eq!(imported[0].bytes, b"fake attachment bytes");
        assert!(response.contains("available at @telegram_uploads/msg-1/1-cat.jpg"));
    }

    #[tokio::test]
    async fn addressed_message_should_import_replied_attachment() {
        let (app, runtime, mut messages) = app_with_fakes().await;
        let mut message = incoming("@telellm_bot what is this?");
        message.reply_to = Some(crate::bot::message::RepliedMessage {
            message_id: MessageId(7),
            from_name: Some("Mike".to_owned()),
            text: String::new(),
            attachments: vec![IncomingAttachment::new(
                AttachmentKind::Document,
                "telegram-file-id".to_owned(),
                "telegram-unique-id".to_owned(),
                Some("diagram.png".to_owned()),
                Some("image/png".to_owned()),
                15,
            )],
        });

        app.handle_message(message)
            .await
            .expect("addressed message should be handled");
        let response = messages.recv().await.expect("response should be sent");
        let imported = runtime.imported.lock().await;

        assert_eq!(imported.len(), 1);
        assert_eq!(
            imported[0].workspace_path,
            "telegram_uploads/msg-7/1-diagram.png"
        );
        assert!(response.contains("Reply context:\n- Mike: (no text)"));
        assert!(response.contains("available at @telegram_uploads/msg-7/1-diagram.png"));
    }

    async fn app_with_fakes() -> (
        AppCore<FakeMemoryStore, FakeCodexSession, FakeTelegram, FakeRuntime>,
        Arc<FakeRuntime>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let (app, runtime, messages, _codex) = app_with_fakes_and_session().await;
        (app, runtime, messages)
    }

    async fn app_with_fakes_and_session() -> (
        AppCore<FakeMemoryStore, FakeCodexSession, FakeTelegram, FakeRuntime>,
        Arc<FakeRuntime>,
        mpsc::UnboundedReceiver<String>,
        Arc<FakeCodexSession>,
    ) {
        let memory = Arc::new(FakeMemoryStore::default());
        let (telegram, messages) = FakeTelegram::new();
        let router = Arc::new(Router::new(4, telegram));
        let codex = Arc::new(FakeCodexSession::default());
        router.register_session(ChatId(1), 1, codex.clone()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let app = AppCore::new(
            app_core_config(Vec::new(), true),
            memory,
            RollingBuffer::new(10),
            router,
            runtime.clone(),
        );
        (app, runtime, messages, codex)
    }

    async fn app_with_fakes_and_allowed_chats(
        allowed_chat_ids: Vec<ChatId>,
    ) -> (
        AppCore<FakeMemoryStore, FakeCodexSession, FakeTelegram, FakeRuntime>,
        Arc<FakeRuntime>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let memory = Arc::new(FakeMemoryStore::default());
        let (telegram, messages) = FakeTelegram::new();
        let router = Arc::new(Router::new(4, telegram));
        router
            .register_session(ChatId(1), 1, Arc::new(FakeCodexSession::default()))
            .await;
        let runtime = Arc::new(FakeRuntime::default());
        let app = AppCore::new(
            app_core_config(allowed_chat_ids, true),
            memory,
            RollingBuffer::new(10),
            router,
            runtime.clone(),
        );
        (app, runtime, messages)
    }

    async fn app_with_failing_runtime() -> (
        AppCore<FakeMemoryStore, FakeCodexSession, FakeTelegram, FakeRuntime>,
        Arc<FakeRuntime>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let (app, _runtime, messages) = app_with_fakes().await;
        let runtime = app.runtime.clone();
        runtime.fail_ensure.store(true, Ordering::SeqCst);
        (app, runtime, messages)
    }

    #[derive(Default)]
    struct FakeMemoryStore {
        forgotten: AtomicUsize,
        remembered: AtomicUsize,
    }

    #[async_trait]
    impl MemoryStore for FakeMemoryStore {
        async fn add_memory(
            &self,
            chat_id: ChatId,
            user_id: Option<UserId>,
            kind: MemoryKind,
            content: &str,
        ) -> Result<MemoryRecord, MemoryStoreError> {
            self.remembered.fetch_add(1, Ordering::SeqCst);
            Ok(MemoryRecord {
                id: 1,
                chat_id,
                user_id,
                kind,
                content: content.to_owned(),
            })
        }

        async fn list_memories(
            &self,
            chat_id: ChatId,
        ) -> Result<Vec<MemoryRecord>, MemoryStoreError> {
            Ok(vec![MemoryRecord {
                id: 1,
                chat_id,
                user_id: Some(UserId(2)),
                kind: MemoryKind::Preference,
                content: "Mike likes short answers".to_owned(),
            }])
        }

        async fn forget_all(&self, _chat_id: ChatId) -> Result<u64, MemoryStoreError> {
            self.forgotten.fetch_add(1, Ordering::SeqCst);
            Ok(1)
        }
    }

    #[derive(Default)]
    struct FakeCodexSession {
        prompts: AtomicUsize,
    }

    #[async_trait]
    impl CodexSession for FakeCodexSession {
        async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
            self.prompts.fetch_add(1, Ordering::SeqCst);
            Ok(CodexTurn::text(request.prompt))
        }

        async fn restart(&self) -> Result<(), CodexSessionError> {
            Ok(())
        }
    }

    struct BlockingCodexSession {
        entered: mpsc::UnboundedSender<String>,
        release: tokio::sync::Notify,
    }

    impl BlockingCodexSession {
        fn new(entered: mpsc::UnboundedSender<String>) -> Self {
            Self {
                entered,
                release: tokio::sync::Notify::new(),
            }
        }
    }

    #[async_trait]
    impl CodexSession for BlockingCodexSession {
        async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
            self.entered
                .send(request.prompt.clone())
                .map_err(|err| CodexSessionError::Pty(err.to_string()))?;
            self.release.notified().await;

            Ok(CodexTurn::text(request.prompt))
        }

        async fn restart(&self) -> Result<(), CodexSessionError> {
            Ok(())
        }
    }

    struct FakeTelegram {
        tx: mpsc::UnboundedSender<String>,
    }

    impl FakeTelegram {
        fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
            let (tx, rx) = mpsc::unbounded_channel();
            (Arc::new(Self { tx }), rx)
        }
    }

    #[async_trait]
    impl TelegramSink for FakeTelegram {
        async fn send_message(&self, _chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
            self.tx
                .send(text.to_owned())
                .map_err(|err| TelegramError::Send(err.to_string()))
        }

        async fn download_file_to_path(
            &self,
            _file_id: &str,
            destination: &std::path::Path,
        ) -> Result<u64, TelegramError> {
            tokio::fs::write(destination, b"fake attachment bytes")
                .await
                .map_err(|err| TelegramError::Download(err.to_string()))?;
            Ok(21)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ImportedAttachment {
        workspace_path: String,
        bytes: Vec<u8>,
    }

    struct FakeRuntime {
        ensure_calls: AtomicUsize,
        reset_calls: AtomicUsize,
        fail_ensure: std::sync::atomic::AtomicBool,
        imported: Mutex<Vec<ImportedAttachment>>,
        status: Mutex<ChatRuntimeStatus>,
    }

    impl Default for FakeRuntime {
        fn default() -> Self {
            Self {
                ensure_calls: AtomicUsize::new(0),
                reset_calls: AtomicUsize::new(0),
                fail_ensure: std::sync::atomic::AtomicBool::new(false),
                imported: Mutex::new(Vec::new()),
                status: Mutex::new(ChatRuntimeStatus {
                    chat_id: ChatId(1),
                    state: RuntimeState::Ready,
                    generation: 1,
                }),
            }
        }
    }

    impl FakeRuntime {
        async fn set_status(&self, status: ChatRuntimeStatus) {
            *self.status.lock().await = status;
        }
    }

    #[async_trait]
    impl RuntimeControl for FakeRuntime {
        async fn ensure_chat_runtime(
            &self,
            _chat_id: ChatId,
        ) -> Result<(), crate::runtime::RuntimeError> {
            self.ensure_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_ensure.load(Ordering::SeqCst) {
                return Err(crate::runtime::RuntimeError::InvalidBrokerUrl(
                    "test runtime failure".to_owned(),
                ));
            }
            Ok(())
        }

        async fn import_chat_attachment(
            &self,
            _chat_id: ChatId,
            source_path: &std::path::Path,
            workspace_path: &str,
        ) -> Result<(), crate::runtime::RuntimeError> {
            let bytes = tokio::fs::read(source_path)
                .await
                .map_err(crate::sandbox::SandboxError::Io)?;
            self.imported.lock().await.push(ImportedAttachment {
                workspace_path: workspace_path.to_owned(),
                bytes,
            });
            Ok(())
        }

        async fn reset_chat_runtime(
            &self,
            _chat_id: ChatId,
            _clear_workspace: bool,
        ) -> Result<(), crate::runtime::RuntimeError> {
            self.reset_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn restart_chat_runtime(
            &self,
            _chat_id: ChatId,
        ) -> Result<(), crate::runtime::RuntimeError> {
            Ok(())
        }

        async fn rebuild_chat_runtime(
            &self,
            _chat_id: ChatId,
            _clear_workspace: bool,
        ) -> Result<(), crate::runtime::RuntimeError> {
            Ok(())
        }

        async fn chat_status(&self, chat_id: ChatId) -> ChatRuntimeStatus {
            let mut status = self.status.lock().await.clone();
            status.chat_id = chat_id;
            status
        }
    }
}
