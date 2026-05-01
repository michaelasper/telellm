use crate::{
    bot::{
        command::{BotCommand, ForgetTarget},
        message::{Addressing, IncomingMessage},
        telegram::{IncomingMessageHandler, TelegramError},
    },
    config::{AppConfig, CodexAuthMode},
    memory::{MemoryKind, MemoryStore, RollingBuffer, context::ContextPacket},
    router::{GroupWorkItem, Router, RouterError},
    runtime::{ChatRuntimeStatus, RuntimeControl, RuntimeState},
};
use anyhow::Context;
use async_trait::async_trait;
use secrecy::ExposeSecret;
use std::sync::Arc;
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
        config.broker.clone(),
        router.clone(),
        crate::config::codex_read_policy(&config),
    ));
    let app_core = Arc::new(AppCore::new(
        config.telegram.bot_username.clone(),
        config.prompt.system_prompt.clone(),
        allowed_chat_ids,
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
    memory_store: Arc<M>,
    rolling: Arc<Mutex<RollingBuffer>>,
    router: Arc<Router<S, T>>,
    runtime: Arc<N>,
}

impl<M, S, T, N> AppCore<M, S, T, N>
where
    M: MemoryStore + 'static,
    S: crate::codex::session::CodexSession + 'static,
    T: crate::bot::telegram::TelegramSink + 'static,
    N: RuntimeControl + 'static,
{
    pub fn new(
        bot_username: String,
        system_prompt: String,
        allowed_chat_ids: Vec<crate::ids::ChatId>,
        memory_store: Arc<M>,
        rolling: RollingBuffer,
        router: Arc<Router<S, T>>,
        runtime: Arc<N>,
    ) -> Self {
        Self {
            bot_username,
            system_prompt,
            allowed_chat_ids,
            memory_store,
            rolling: Arc::new(Mutex::new(rolling)),
            router,
            runtime,
        }
    }

    pub async fn handle_message(&self, message: IncomingMessage) -> Result<(), AppError> {
        self.rolling.lock().await.push(message.clone());
        let command = BotCommand::parse(&message.text, &self.bot_username)?;

        if !self.chat_allowed(message.chat_id) {
            return Ok(());
        }

        if command.is_none() && message.addressing(&self.bot_username) == Addressing::Ambient {
            return Ok(());
        }

        if let Some(command) = command {
            return self.handle_command(message, command).await;
        }

        self.runtime.ensure_chat_runtime(message.chat_id).await?;

        let recent_messages = self.rolling.lock().await.recent_for_chat(message.chat_id);
        let memories = self.memory_store.list_memories(message.chat_id).await?;
        let packet = ContextPacket {
            system_prompt: self.system_prompt.clone(),
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
        if receipt.queue_position > 0 {
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
            reply_to_bot: false,
            private_chat: false,
        }
    }

    fn test_system_prompt() -> String {
        "Test system prompt.".to_owned()
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
            "telellm_bot".to_owned(),
            test_system_prompt(),
            Vec::new(),
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
            "telellm_bot".to_owned(),
            test_system_prompt(),
            Vec::new(),
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
            "telellm_bot".to_owned(),
            test_system_prompt(),
            allowed_chat_ids,
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
            Ok(CodexTurn {
                output: request.prompt,
            })
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

            Ok(CodexTurn {
                output: request.prompt,
            })
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
    }

    struct FakeRuntime {
        ensure_calls: AtomicUsize,
        reset_calls: AtomicUsize,
        fail_ensure: std::sync::atomic::AtomicBool,
        status: Mutex<ChatRuntimeStatus>,
    }

    impl Default for FakeRuntime {
        fn default() -> Self {
            Self {
                ensure_calls: AtomicUsize::new(0),
                reset_calls: AtomicUsize::new(0),
                fail_ensure: std::sync::atomic::AtomicBool::new(false),
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
