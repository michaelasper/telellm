use crate::{
    bot::{
        command::{BotCommand, ForgetTarget},
        message::{Addressing, IncomingMessage},
        telegram::IncomingMessageHandler,
    },
    config::AppConfig,
    memory::{MemoryStore, RollingBuffer, context::ContextPacket},
    router::{GroupWorkItem, Router, RouterError},
    runtime::RuntimeControl,
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

    let upstream_api_key =
        std::env::var(&config.broker.upstream_api_key_env).with_context(|| {
            format!(
                "required upstream API key env var `{}` is not set",
                config.broker.upstream_api_key_env
            )
        })?;
    let broker_state = crate::broker::BrokerState::new(crate::broker::BrokerConfig {
        listen: config.broker.listen,
        upstream_base_url: config.broker.upstream_base_url.clone(),
        upstream_api_key: secrecy::SecretString::from(upstream_api_key),
    });
    let broker_listener = tokio::net::TcpListener::bind(config.broker.listen)
        .await
        .with_context(|| format!("failed to bind broker at {}", config.broker.listen))?;
    tokio::spawn(async move {
        if let Err(err) = axum::serve(broker_listener, crate::broker::router(broker_state)).await {
            tracing::error!(error = %err, "broker server stopped");
        }
    });

    let telegram_token = config.telegram_token_from_env()?;
    let bot = teloxide::Bot::new(telegram_token.expose_secret().to_owned());
    let telegram_sink = Arc::new(crate::bot::telegram::TeloxideTelegramSink::new(
        bot.clone(),
        config.limits.telegram_chunk_chars,
    ));
    let router = Arc::new(crate::router::Router::new(
        config.limits.per_group_queue_depth,
        telegram_sink,
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
        memory_store: Arc<M>,
        rolling: RollingBuffer,
        router: Arc<Router<S, T>>,
        runtime: Arc<N>,
    ) -> Self {
        Self {
            bot_username,
            memory_store,
            rolling: Arc::new(Mutex::new(rolling)),
            router,
            runtime,
        }
    }

    pub async fn handle_message(&self, message: IncomingMessage) -> Result<(), AppError> {
        self.rolling.lock().await.push(message.clone());
        let command = BotCommand::parse(&message.text, &self.bot_username)?;

        if command.is_none() && message.addressing(&self.bot_username) == Addressing::Ambient {
            return Ok(());
        }

        self.runtime.ensure_chat_runtime(message.chat_id).await?;

        if let Some(command) = command {
            return self.handle_command(message, command).await;
        }

        let recent_messages = self.rolling.lock().await.recent_for_chat(message.chat_id);
        let memories = self.memory_store.list_memories(message.chat_id).await?;
        let packet = ContextPacket {
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
                self.enqueue_text(message.chat_id, help_text()).await?;
            }
            BotCommand::Status => {
                self.enqueue_text(
                    message.chat_id,
                    "Report concise status for this group's sandbox, Codex session, queue, and memory.",
                )
                .await?;
            }
            BotCommand::Reset { clear_workspace } => {
                self.runtime
                    .reset_chat_runtime(message.chat_id, clear_workspace)
                    .await?;
                self.enqueue_text(message.chat_id, "The group Codex session has been reset.")
                    .await?;
            }
            BotCommand::Restart => {
                self.runtime.restart_chat_runtime(message.chat_id).await?;
                self.enqueue_text(message.chat_id, "The group sandbox has been restarted.")
                    .await?;
            }
            BotCommand::Rebuild { clear_workspace } => {
                self.runtime
                    .rebuild_chat_runtime(message.chat_id, clear_workspace)
                    .await?;
                self.enqueue_text(message.chat_id, "The group sandbox has been rebuilt.")
                    .await?;
            }
            BotCommand::Memory => {
                let memories = self.memory_store.list_memories(message.chat_id).await?;
                self.enqueue_text(
                    message.chat_id,
                    format!("Summarize these durable group memories: {memories:?}"),
                )
                .await?;
            }
            BotCommand::Forget { target } => match target {
                ForgetTarget::All => {
                    let removed = self.memory_store.forget_all(message.chat_id).await?;
                    self.enqueue_text(
                        message.chat_id,
                        format!("Forgot {removed} durable memory records for this group."),
                    )
                    .await?;
                }
                ForgetTarget::Query(query) => {
                    self.enqueue_text(
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

    async fn enqueue_text(
        &self,
        chat_id: crate::ids::ChatId,
        prompt: impl Into<String>,
    ) -> Result<(), AppError> {
        self.router
            .enqueue(GroupWorkItem {
                chat_id,
                prompt: prompt.into(),
            })
            .await?;
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
        if let Err(err) = AppCore::handle_message(self, message).await {
            tracing::warn!(error = %err, "failed to handle Telegram message");
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
}

fn help_text() -> &'static str {
    "Show concise help for /status /reset /restart /rebuild /memory /forget."
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
    use tokio::sync::mpsc;

    fn incoming(text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id: ChatId(1),
            message_id: MessageId(1),
            from: Some(UserId(2)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            reply_to_bot: false,
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
    async fn handle_message_should_enqueue_addressed_context() {
        let (app, runtime, mut messages) = app_with_fakes().await;

        app.handle_message(incoming("@telellm_bot hello"))
            .await
            .expect("addressed message should be handled");
        let response = messages.recv().await.expect("response should be sent");

        assert!(response.contains("@telellm_bot hello"));
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
        assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 1);
    }

    async fn app_with_fakes() -> (
        AppCore<FakeMemoryStore, FakeCodexSession, FakeTelegram, FakeRuntime>,
        Arc<FakeRuntime>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let memory = Arc::new(FakeMemoryStore::default());
        let (telegram, messages) = FakeTelegram::new();
        let router = Arc::new(Router::new(4, telegram));
        router
            .register_session(ChatId(1), Arc::new(FakeCodexSession))
            .await;
        let runtime = Arc::new(FakeRuntime::default());
        let app = AppCore::new(
            "telellm_bot".to_owned(),
            memory,
            RollingBuffer::new(10),
            router,
            runtime.clone(),
        );
        (app, runtime, messages)
    }

    #[derive(Default)]
    struct FakeMemoryStore {
        forgotten: AtomicUsize,
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

    struct FakeCodexSession;

    #[async_trait]
    impl CodexSession for FakeCodexSession {
        async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
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

    #[derive(Default)]
    struct FakeRuntime {
        ensure_calls: AtomicUsize,
        reset_calls: AtomicUsize,
    }

    #[async_trait]
    impl RuntimeControl for FakeRuntime {
        async fn ensure_chat_runtime(
            &self,
            _chat_id: ChatId,
        ) -> Result<(), crate::runtime::RuntimeError> {
            self.ensure_calls.fetch_add(1, Ordering::SeqCst);
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
    }
}
