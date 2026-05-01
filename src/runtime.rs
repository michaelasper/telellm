use crate::{
    bot::telegram::TelegramSink,
    broker::{BrokerToken, BrokerTokenRegistry},
    codex::{
        exec::CommandCodexSession,
        pty::{PtyCodexSession, PtyReadPolicy},
        session::{CodexRequest, CodexSession, CodexSessionError, CodexTurn},
    },
    config::{BrokerConfig, CodexAuthMode, CodexConfig, DockerConfig},
    ids::ChatId,
    router::Router,
    sandbox::{SandboxBackend, SandboxError, SandboxSpec, docker::DockerSandboxBackend},
};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::Mutex;

const CODEX_EXEC_WRAPPER: &str = r#"out="$(mktemp "${TMPDIR:-/tmp}/telellm-codex.XXXXXX")"
log="$(mktemp "${TMPDIR:-/tmp}/telellm-codex-log.XXXXXX")"
if "$@" --output-last-message "$out" - >"$log" 2>&1; then
  cat "$out"
  status=0
else
  status=$?
  cat "$log" >&2
fi
rm -f "$out" "$log"
exit "$status"
"#;

#[async_trait]
pub trait RuntimeControl: Send + Sync {
    async fn ensure_chat_runtime(&self, chat_id: ChatId) -> Result<(), RuntimeError>;
    async fn reset_chat_runtime(
        &self,
        chat_id: ChatId,
        clear_workspace: bool,
    ) -> Result<(), RuntimeError>;
    async fn restart_chat_runtime(&self, chat_id: ChatId) -> Result<(), RuntimeError>;
    async fn rebuild_chat_runtime(
        &self,
        chat_id: ChatId,
        clear_workspace: bool,
    ) -> Result<(), RuntimeError>;
    async fn chat_status(&self, chat_id: ChatId) -> ChatRuntimeStatus;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRuntimeStatus {
    pub chat_id: ChatId,
    pub state: RuntimeState,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeState {
    NotStarted,
    Starting,
    Ready,
    Degraded(String),
}

#[derive(Debug, Clone)]
struct RuntimeStatusRecord {
    state: RuntimeState,
    generation: u64,
}

impl Default for RuntimeStatusRecord {
    fn default() -> Self {
        Self {
            state: RuntimeState::NotStarted,
            generation: 0,
        }
    }
}

pub enum ManagedCodexSession {
    Pty(PtyCodexSession),
    Command(CommandCodexSession),
}

#[async_trait]
impl CodexSession for ManagedCodexSession {
    async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
        match self {
            Self::Pty(session) => session.send(request).await,
            Self::Command(session) => session.send(request).await,
        }
    }

    async fn restart(&self) -> Result<(), CodexSessionError> {
        match self {
            Self::Pty(session) => session.restart().await,
            Self::Command(session) => session.restart().await,
        }
    }
}

pub struct RuntimeManager<T>
where
    T: TelegramSink + 'static,
{
    docker: DockerSandboxBackend,
    docker_config: DockerConfig,
    codex_config: CodexConfig,
    broker_config: Option<BrokerConfig>,
    read_policy: PtyReadPolicy,
    router: Arc<Router<ManagedCodexSession, T>>,
    broker_registry: BrokerTokenRegistry,
    broker_tokens: StdMutex<HashMap<ChatId, BrokerToken>>,
    sandbox_auth_states: StdMutex<HashSet<ChatId>>,
    registered: Mutex<HashSet<ChatId>>,
    lifecycle_locks: Mutex<HashMap<ChatId, Arc<Mutex<()>>>>,
    statuses: Mutex<HashMap<ChatId, RuntimeStatusRecord>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxAuthState {
    Existing,
    Generated,
}

impl<T> RuntimeManager<T>
where
    T: TelegramSink + 'static,
{
    pub fn new(
        docker: DockerSandboxBackend,
        docker_config: DockerConfig,
        codex_config: CodexConfig,
        broker_config: Option<BrokerConfig>,
        router: Arc<Router<ManagedCodexSession, T>>,
        read_policy: PtyReadPolicy,
    ) -> Self {
        Self::new_with_broker_registry(
            docker,
            docker_config,
            codex_config,
            broker_config,
            router,
            read_policy,
            BrokerTokenRegistry::global(),
        )
    }

    pub fn new_with_broker_registry(
        docker: DockerSandboxBackend,
        docker_config: DockerConfig,
        codex_config: CodexConfig,
        broker_config: Option<BrokerConfig>,
        router: Arc<Router<ManagedCodexSession, T>>,
        read_policy: PtyReadPolicy,
        broker_registry: BrokerTokenRegistry,
    ) -> Self {
        Self {
            docker,
            docker_config,
            codex_config,
            broker_config,
            read_policy,
            router,
            broker_registry,
            broker_tokens: StdMutex::new(HashMap::new()),
            sandbox_auth_states: StdMutex::new(HashSet::new()),
            registered: Mutex::new(HashSet::new()),
            lifecycle_locks: Mutex::new(HashMap::new()),
            statuses: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn read_policy_for_test(&self) -> &PtyReadPolicy {
        &self.read_policy
    }

    fn spec_for_chat(&self, chat_id: ChatId) -> Result<SandboxSpec, RuntimeError> {
        self.spec_for_chat_with_auth_state(chat_id)
            .map(|(spec, _auth_state)| spec)
    }

    fn spec_for_chat_with_auth_state(
        &self,
        chat_id: ChatId,
    ) -> Result<(SandboxSpec, SandboxAuthState), RuntimeError> {
        let mut spec = SandboxSpec::new(
            chat_id,
            &self.docker_config.image,
            &self.docker_config.network,
            &self.docker_config.workspace_volume_prefix,
        );

        let auth_state = match self.codex_config.auth_mode {
            CodexAuthMode::BrokerApiKey => {
                let broker_config = self
                    .broker_config
                    .as_ref()
                    .ok_or(RuntimeError::MissingBrokerConfig)?;
                let (broker_token, auth_state) = self.broker_token_for_chat(chat_id);
                let broker_url =
                    reqwest::Url::parse(&broker_config.public_base_url).map_err(|_| {
                        RuntimeError::InvalidBrokerUrl(broker_config.public_base_url.clone())
                    })?;
                let broker_host = broker_url
                    .host_str()
                    .ok_or_else(|| {
                        RuntimeError::InvalidBrokerUrl(broker_config.public_base_url.clone())
                    })?
                    .to_owned();
                let broker_port = broker_url
                    .port_or_known_default()
                    .unwrap_or_else(|| broker_config.listen.port());

                spec.broker_host = broker_host;
                spec.broker_port = broker_port;
                spec.broker_base_url = broker_config.public_base_url.clone();
                spec.broker_token = broker_token;
                auth_state
            }
            CodexAuthMode::ChatgptOauth => {
                let auth_host_path = self
                    .codex_config
                    .auth_host_path
                    .clone()
                    .ok_or(RuntimeError::MissingCodexAuthHostPath)?;
                spec.codex_auth_host_path = Some(auth_host_path);
                self.sandbox_auth_state_for_chat(chat_id)
            }
        };

        Ok((spec, auth_state))
    }

    fn broker_token_for_chat(&self, chat_id: ChatId) -> (BrokerToken, SandboxAuthState) {
        let mut broker_tokens = self
            .broker_tokens
            .lock()
            .expect("broker token map mutex should not be poisoned");
        if let Some(token) = broker_tokens.get(&chat_id) {
            return (token.clone(), SandboxAuthState::Existing);
        }

        let token = BrokerToken::generate();
        broker_tokens.insert(chat_id, token.clone());
        (token, SandboxAuthState::Generated)
    }

    fn sandbox_auth_state_for_chat(&self, chat_id: ChatId) -> SandboxAuthState {
        let mut states = self
            .sandbox_auth_states
            .lock()
            .expect("sandbox auth state mutex should not be poisoned");
        if states.insert(chat_id) {
            SandboxAuthState::Generated
        } else {
            SandboxAuthState::Existing
        }
    }

    fn rotate_broker_token_for_chat(&self, chat_id: ChatId) {
        let mut broker_tokens = self
            .broker_tokens
            .lock()
            .expect("broker token map mutex should not be poisoned");
        broker_tokens.insert(chat_id, BrokerToken::generate());
    }

    async fn spawn_and_register_session(&self, spec: &SandboxSpec) -> Result<(), RuntimeError> {
        let session = Arc::new(self.codex_session_for_spec(spec)?);
        if self.codex_config.auth_mode == CodexAuthMode::BrokerApiKey {
            self.broker_registry
                .register_token(spec.chat_id, spec.broker_token.clone())
                .await;
        }
        let generation = self.mark_ready_with_next_generation(spec.chat_id).await;
        self.router
            .register_session(spec.chat_id, generation, session)
            .await;
        self.registered.lock().await.insert(spec.chat_id);
        Ok(())
    }

    fn codex_session_for_spec(
        &self,
        spec: &SandboxSpec,
    ) -> Result<ManagedCodexSession, RuntimeError> {
        if self.codex_uses_noninteractive_exec() {
            return Ok(ManagedCodexSession::Command(CommandCodexSession::new(
                "docker",
                self.codex_exec_docker_args(spec),
                self.read_policy.max_turn_timeout,
                self.read_policy.max_output_bytes,
            )));
        }

        Ok(ManagedCodexSession::Pty(
            PtyCodexSession::spawn_with_read_policy(
                "docker",
                &self.codex_pty_docker_args(spec),
                self.read_policy.clone(),
            )?,
        ))
    }

    fn codex_uses_noninteractive_exec(&self) -> bool {
        self.codex_config
            .args
            .first()
            .is_some_and(|arg| arg == "exec")
    }

    fn codex_command_parts(&self) -> Vec<String> {
        let mut codex_parts = Vec::with_capacity(self.codex_config.args.len() + 5);
        codex_parts.push(self.codex_config.command.clone());
        codex_parts.extend(self.codex_config.args.iter().cloned());
        codex_parts.extend([
            "--model".to_owned(),
            self.codex_config.model.clone(),
            "--cd".to_owned(),
            "/workspace".to_owned(),
        ]);
        codex_parts
    }

    fn codex_pty_docker_args(&self, spec: &SandboxSpec) -> Vec<String> {
        let codex_parts = self.codex_command_parts();
        let command: Vec<&str> = codex_parts.iter().map(String::as_str).collect();
        DockerSandboxBackend::exec_args(spec, &command)
    }

    fn codex_exec_docker_args(&self, spec: &SandboxSpec) -> Vec<String> {
        let mut command = vec!["sh", "-lc", CODEX_EXEC_WRAPPER, "telellm-codex-exec"];
        let codex_parts = self.codex_command_parts();
        command.extend(codex_parts.iter().map(String::as_str));
        DockerSandboxBackend::exec_no_tty_args(spec, &command)
    }

    async fn ensure_sandbox_started_for_auth(
        &self,
        spec: &SandboxSpec,
        auth_state: SandboxAuthState,
    ) -> Result<(), RuntimeError> {
        if should_recreate_sandbox_for_auth_state(auth_state) {
            self.docker.rebuild(spec, false).await?;
        } else {
            self.docker.ensure_started(spec).await?;
        }
        Ok(())
    }

    async fn ensure_chat_runtime_inner(&self, chat_id: ChatId) -> Result<(), RuntimeError> {
        self.ensure_unregistered_chat_runtime(chat_id, async {
            let (spec, auth_state) = self.spec_for_chat_with_auth_state(chat_id)?;
            self.ensure_sandbox_started_for_auth(&spec, auth_state)
                .await?;
            self.spawn_and_register_session(&spec).await
        })
        .await
    }

    async fn ensure_unregistered_chat_runtime(
        &self,
        chat_id: ChatId,
        start_runtime: impl Future<Output = Result<(), RuntimeError>>,
    ) -> Result<(), RuntimeError> {
        self.with_chat_lifecycle_lock(chat_id, async move {
            if self.registered.lock().await.contains(&chat_id) {
                return Ok(());
            }

            self.set_runtime_state(chat_id, RuntimeState::Starting)
                .await;
            let result = start_runtime.await;
            self.record_lifecycle_result(chat_id, &result).await;
            result
        })
        .await
    }

    async fn replace_chat_session(
        &self,
        chat_id: ChatId,
        replace_session: impl Future<Output = Result<(), RuntimeError>>,
    ) -> Result<(), RuntimeError> {
        self.with_chat_lifecycle_lock(chat_id, async move {
            self.set_runtime_state(chat_id, RuntimeState::Starting)
                .await;
            self.router.mark_session_stale(chat_id).await;
            self.registered.lock().await.remove(&chat_id);
            let result = replace_session.await;
            self.record_lifecycle_result(chat_id, &result).await;
            result
        })
        .await
    }

    #[cfg(test)]
    async fn restart_chat_runtime_for_test(
        &self,
        chat_id: ChatId,
        restart_runtime: impl Future<Output = Result<(), RuntimeError>>,
    ) -> Result<(), RuntimeError> {
        self.replace_chat_session(chat_id, restart_runtime).await
    }

    async fn with_chat_lifecycle_lock<R>(
        &self,
        chat_id: ChatId,
        operation: impl Future<Output = R>,
    ) -> R {
        let lifecycle_lock = {
            let mut lifecycle_locks = self.lifecycle_locks.lock().await;
            lifecycle_locks
                .entry(chat_id)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let lifecycle_guard = lifecycle_lock.lock().await;

        let result = operation.await;
        drop(lifecycle_guard);

        self.remove_unused_lifecycle_lock(chat_id, &lifecycle_lock)
            .await;
        result
    }

    async fn remove_unused_lifecycle_lock(&self, chat_id: ChatId, lifecycle_lock: &Arc<Mutex<()>>) {
        let mut lifecycle_locks = self.lifecycle_locks.lock().await;
        let should_remove = lifecycle_locks.get(&chat_id).is_some_and(|current| {
            Arc::ptr_eq(current, lifecycle_lock) && Arc::strong_count(lifecycle_lock) == 2
        });
        if should_remove {
            lifecycle_locks.remove(&chat_id);
        }
    }

    async fn mark_ready_with_next_generation(&self, chat_id: ChatId) -> u64 {
        let mut statuses = self.statuses.lock().await;
        let status = statuses.entry(chat_id).or_default();
        status.generation += 1;
        status.state = RuntimeState::Ready;
        status.generation
    }

    async fn set_runtime_state(&self, chat_id: ChatId, state: RuntimeState) {
        self.statuses.lock().await.entry(chat_id).or_default().state = state;
    }

    async fn record_lifecycle_result(&self, chat_id: ChatId, result: &Result<(), RuntimeError>) {
        match result {
            Ok(()) => {
                let registered = self.registered.lock().await.contains(&chat_id);
                if registered {
                    self.set_runtime_state(chat_id, RuntimeState::Ready).await;
                }
            }
            Err(error) => {
                self.set_runtime_state(chat_id, RuntimeState::Degraded(error.to_string()))
                    .await;
            }
        }
    }

    async fn chat_status_inner(&self, chat_id: ChatId) -> ChatRuntimeStatus {
        let status = self
            .statuses
            .lock()
            .await
            .get(&chat_id)
            .cloned()
            .unwrap_or_default();
        ChatRuntimeStatus {
            chat_id,
            state: status.state,
            generation: status.generation,
        }
    }

    async fn reset_chat_runtime_inner(
        &self,
        chat_id: ChatId,
        clear_workspace: bool,
    ) -> Result<(), RuntimeError> {
        self.replace_chat_session(chat_id, async move {
            if clear_workspace && self.codex_config.auth_mode == CodexAuthMode::BrokerApiKey {
                self.rotate_broker_token_for_chat(chat_id);
                self.broker_registry.unregister_chat(chat_id).await;
            }
            let (spec, auth_state) = self.spec_for_chat_with_auth_state(chat_id)?;
            if clear_workspace {
                self.docker.rebuild(&spec, true).await?;
            } else {
                self.ensure_sandbox_started_for_auth(&spec, auth_state)
                    .await?;
            }
            self.spawn_and_register_session(&spec).await
        })
        .await
    }

    async fn restart_chat_runtime_inner(&self, chat_id: ChatId) -> Result<(), RuntimeError> {
        self.replace_chat_session(chat_id, async move {
            let (spec, auth_state) = self.spec_for_chat_with_auth_state(chat_id)?;
            if should_recreate_sandbox_for_auth_state(auth_state) {
                self.docker.rebuild(&spec, false).await?;
            } else {
                self.docker.restart(&spec).await?;
            }
            self.spawn_and_register_session(&spec).await
        })
        .await
    }

    async fn rebuild_chat_runtime_inner(
        &self,
        chat_id: ChatId,
        clear_workspace: bool,
    ) -> Result<(), RuntimeError> {
        self.replace_chat_session(chat_id, async move {
            if self.codex_config.auth_mode == CodexAuthMode::BrokerApiKey {
                self.rotate_broker_token_for_chat(chat_id);
                self.broker_registry.unregister_chat(chat_id).await;
            }
            let spec = self.spec_for_chat(chat_id)?;
            self.docker.rebuild(&spec, clear_workspace).await?;
            self.spawn_and_register_session(&spec).await
        })
        .await
    }
}

fn should_recreate_sandbox_for_auth_state(auth_state: SandboxAuthState) -> bool {
    auth_state == SandboxAuthState::Generated
}

#[async_trait]
impl<T> RuntimeControl for RuntimeManager<T>
where
    T: TelegramSink + 'static,
{
    async fn ensure_chat_runtime(&self, chat_id: ChatId) -> Result<(), RuntimeError> {
        self.ensure_chat_runtime_inner(chat_id).await
    }

    async fn reset_chat_runtime(
        &self,
        chat_id: ChatId,
        clear_workspace: bool,
    ) -> Result<(), RuntimeError> {
        self.reset_chat_runtime_inner(chat_id, clear_workspace)
            .await
    }

    async fn restart_chat_runtime(&self, chat_id: ChatId) -> Result<(), RuntimeError> {
        self.restart_chat_runtime_inner(chat_id).await
    }

    async fn rebuild_chat_runtime(
        &self,
        chat_id: ChatId,
        clear_workspace: bool,
    ) -> Result<(), RuntimeError> {
        self.rebuild_chat_runtime_inner(chat_id, clear_workspace)
            .await
    }

    async fn chat_status(&self, chat_id: ChatId) -> ChatRuntimeStatus {
        self.chat_status_inner(chat_id).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("sandbox error: {0}")]
    Sandbox(#[from] SandboxError),
    #[error("Codex session error: {0}")]
    Codex(#[from] crate::codex::session::CodexSessionError),
    #[error("broker public_base_url is invalid: {0}")]
    InvalidBrokerUrl(String),
    #[error("broker config is required when codex auth mode is broker_api_key")]
    MissingBrokerConfig,
    #[error("codex.auth_host_path is required when codex auth mode is chatgpt_oauth")]
    MissingCodexAuthHostPath,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AppConfig, BrokerConfig, CodexAuthMode, CodexConfig, DockerConfig, LimitsConfig,
        StorageConfig, TelegramConfig, codex_read_policy,
    };
    use std::{
        collections::BTreeMap,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::sync::{Barrier, Notify};

    fn configs() -> (DockerConfig, CodexConfig, BrokerConfig) {
        (
            DockerConfig {
                image: "telellm-sandbox:local".to_owned(),
                network: "telellm_public".to_owned(),
                workspace_volume_prefix: "telellm_workspace".to_owned(),
            },
            CodexConfig {
                command: "codex".to_owned(),
                args: vec!["--no-alt-screen".to_owned()],
                model: "gpt-5-codex".to_owned(),
                auth_mode: CodexAuthMode::BrokerApiKey,
                auth_host_path: None,
                env: BTreeMap::new(),
            },
            BrokerConfig {
                listen: "127.0.0.1:8189"
                    .parse()
                    .expect("listen address should parse"),
                public_base_url: "http://host.docker.internal:8189/v1".to_owned(),
                upstream_base_url: "https://api.openai.com/v1".to_owned(),
                upstream_api_key_env: "OPENAI_API_KEY".to_owned(),
            },
        )
    }

    #[test]
    fn spec_for_chat_should_use_configured_broker_url() {
        let (docker, codex, broker) = configs();
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            Some(broker),
            Arc::new(router),
            PtyReadPolicy::default(),
        );

        let spec = manager.spec_for_chat(ChatId(1)).expect("spec should build");

        assert_eq!(spec.broker_host, "host.docker.internal");
    }

    #[test]
    fn spec_for_chat_should_use_unique_broker_token() {
        let (docker, codex, broker) = configs();
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            Some(broker),
            Arc::new(router),
            PtyReadPolicy::default(),
        );

        let first = manager.spec_for_chat(ChatId(1)).expect("spec should build");
        let second = manager.spec_for_chat(ChatId(2)).expect("spec should build");

        assert!(!first.broker_token.is_empty());
        assert!(!second.broker_token.is_empty());
        assert_ne!(first.broker_token, second.broker_token);
    }

    #[test]
    fn spec_for_chat_should_mount_codex_auth_for_chatgpt_oauth_without_broker_config() {
        let (docker, mut codex, _broker) = configs();
        codex.auth_mode = CodexAuthMode::ChatgptOauth;
        codex.auth_host_path = Some(PathBuf::from("/home/user/.codex/auth.json"));
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            None,
            Arc::new(router),
            PtyReadPolicy::default(),
        );

        let spec = manager.spec_for_chat(ChatId(1)).expect("spec should build");

        assert_eq!(
            spec.codex_auth_host_path,
            Some(PathBuf::from("/home/user/.codex/auth.json"))
        );
    }

    #[test]
    fn spec_for_chat_should_reject_chatgpt_oauth_without_auth_host_path() {
        let (docker, mut codex, _broker) = configs();
        codex.auth_mode = CodexAuthMode::ChatgptOauth;
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            None,
            Arc::new(router),
            PtyReadPolicy::default(),
        );

        let err = manager
            .spec_for_chat(ChatId(1))
            .expect_err("spec should be invalid");

        assert_eq!(
            err.to_string(),
            "codex.auth_host_path is required when codex auth mode is chatgpt_oauth"
        );
    }

    #[test]
    fn spec_for_chat_should_reject_broker_api_key_without_broker_config() {
        let (docker, codex, _broker) = configs();
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            None,
            Arc::new(router),
            PtyReadPolicy::default(),
        );

        let err = manager
            .spec_for_chat(ChatId(1))
            .expect_err("spec should be invalid");

        assert_eq!(
            err.to_string(),
            "broker config is required when codex auth mode is broker_api_key"
        );
    }

    #[test]
    fn spec_for_chat_should_mark_first_process_token_as_generated() {
        let (docker, codex, broker) = configs();
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            Some(broker),
            Arc::new(router),
            PtyReadPolicy::default(),
        );

        let (_first, first_auth_state) = manager
            .spec_for_chat_with_auth_state(ChatId(1))
            .expect("spec should build");
        let (_second, second_auth_state) = manager
            .spec_for_chat_with_auth_state(ChatId(1))
            .expect("spec should build");

        assert_eq!(first_auth_state, SandboxAuthState::Generated);
        assert_eq!(second_auth_state, SandboxAuthState::Existing);
    }

    #[test]
    fn chatgpt_oauth_first_process_auth_state_should_recreate_sandbox() {
        let (docker, mut codex, _broker) = configs();
        codex.auth_mode = CodexAuthMode::ChatgptOauth;
        codex.auth_host_path = Some(PathBuf::from("/home/user/.codex/auth.json"));
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            None,
            Arc::new(router),
            PtyReadPolicy::default(),
        );

        let (_first, first_auth_state) = manager
            .spec_for_chat_with_auth_state(ChatId(1))
            .expect("spec should build");
        let (_second, second_auth_state) = manager
            .spec_for_chat_with_auth_state(ChatId(1))
            .expect("spec should build");

        assert!(should_recreate_sandbox_for_auth_state(first_auth_state));
        assert!(!should_recreate_sandbox_for_auth_state(second_auth_state));
    }

    #[test]
    fn newly_generated_broker_token_should_recreate_sandbox() {
        assert!(should_recreate_sandbox_for_auth_state(
            SandboxAuthState::Generated
        ));
        assert!(!should_recreate_sandbox_for_auth_state(
            SandboxAuthState::Existing
        ));
    }

    #[test]
    fn codex_exec_args_should_include_model_and_workspace() {
        let (docker, codex, broker) = configs();
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            Some(broker),
            Arc::new(router),
            PtyReadPolicy::default(),
        );
        let spec = manager.spec_for_chat(ChatId(1)).expect("spec should build");
        let mut parts = Vec::new();
        parts.push(manager.codex_config.command.as_str());
        parts.extend(manager.codex_config.args.iter().map(String::as_str));
        parts.extend([
            "--model",
            manager.codex_config.model.as_str(),
            "--cd",
            "/workspace",
        ]);

        let args = DockerSandboxBackend::exec_args(&spec, &parts);

        assert!(args.contains(&"gpt-5-codex".to_owned()));
    }

    #[test]
    fn codex_exec_mode_should_use_noninteractive_docker_exec() {
        let (docker, mut codex, broker) = configs();
        codex.args = vec![
            "exec".to_owned(),
            "--sandbox".to_owned(),
            "danger-full-access".to_owned(),
            "--skip-git-repo-check".to_owned(),
        ];
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            Some(broker),
            Arc::new(router),
            PtyReadPolicy::default(),
        );
        let spec = manager.spec_for_chat(ChatId(1)).expect("spec should build");

        let args = manager.codex_exec_docker_args(&spec);

        assert!(manager.codex_uses_noninteractive_exec());
        assert!(!args.contains(&"-t".to_owned()));
        assert!(args.contains(&"exec".to_owned()));
        assert!(args.iter().any(|arg| arg.contains("--output-last-message")));
        assert!(args.contains(&"gpt-5-codex".to_owned()));
    }

    #[test]
    fn interactive_codex_mode_should_keep_tty() {
        let (docker, codex, broker) = configs();
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            Some(broker),
            Arc::new(router),
            PtyReadPolicy::default(),
        );
        let spec = manager.spec_for_chat(ChatId(1)).expect("spec should build");

        let args = manager.codex_pty_docker_args(&spec);

        assert!(!manager.codex_uses_noninteractive_exec());
        assert!(args.contains(&"-t".to_owned()));
    }

    #[test]
    fn codex_session_should_use_configured_inactivity_timeout() {
        let (docker, codex, broker) = configs();
        let config = AppConfig {
            telegram: TelegramConfig {
                bot_token_env: "TELEGRAM_BOT_TOKEN".to_owned(),
                bot_username: "telellm_bot".to_owned(),
                allowed_chat_ids: Vec::new(),
            },
            prompt: crate::config::PromptConfig::default(),
            storage: StorageConfig {
                sqlite_path: "data/telellm.sqlite".into(),
            },
            docker: docker.clone(),
            codex: codex.clone(),
            broker: Some(broker.clone()),
            limits: LimitsConfig {
                codex_first_byte_timeout_secs: 13,
                codex_inactivity_secs: 17,
                codex_max_turn_secs: 19,
                codex_max_output_bytes: 23,
                ..LimitsConfig::default()
            },
        };
        let read_policy = codex_read_policy(&config);
        let (_telegram, router) = fake_runtime_router();

        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            Some(broker),
            Arc::new(router),
            read_policy,
        );

        assert_eq!(
            manager.read_policy_for_test().inactivity_timeout,
            Duration::from_secs(17)
        );
        assert_eq!(
            manager.read_policy_for_test().first_byte_timeout,
            Duration::from_secs(13)
        );
        assert_eq!(
            manager.read_policy_for_test().max_turn_timeout,
            Duration::from_secs(19)
        );
        assert_eq!(manager.read_policy_for_test().max_output_bytes, 23);
    }

    #[tokio::test]
    async fn ensure_unregistered_chat_runtime_should_serialize_same_chat_cold_start() {
        let manager = Arc::new(runtime_manager());
        let start_calls = Arc::new(AtomicUsize::new(0));
        let first_started = Arc::new(Notify::new());
        let second_started = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());

        let first_manager = manager.clone();
        let first_marker = manager.clone();
        let first_calls = start_calls.clone();
        let first_started_signal = first_started.clone();
        let first_release = release_first.clone();
        let first = tokio::spawn(async move {
            first_manager
                .ensure_unregistered_chat_runtime(ChatId(1), async move {
                    first_calls.fetch_add(1, Ordering::SeqCst);
                    first_started_signal.notify_one();
                    first_release.notified().await;
                    first_marker.registered.lock().await.insert(ChatId(1));
                    Ok(())
                })
                .await
        });

        first_started.notified().await;

        let second_manager = manager.clone();
        let second_marker = manager.clone();
        let second_calls = start_calls.clone();
        let second_started_signal = second_started.clone();
        let second = tokio::spawn(async move {
            second_manager
                .ensure_unregistered_chat_runtime(ChatId(1), async move {
                    second_calls.fetch_add(1, Ordering::SeqCst);
                    second_started_signal.notify_one();
                    second_marker.registered.lock().await.insert(ChatId(1));
                    Ok(())
                })
                .await
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(25), second_started.notified())
                .await
                .is_err(),
            "same-chat cold start should wait behind the in-flight start"
        );
        assert_eq!(start_calls.load(Ordering::SeqCst), 1);

        release_first.notify_one();
        first
            .await
            .expect("first task should join")
            .expect("first cold start should succeed");
        second
            .await
            .expect("second task should join")
            .expect("second cold start should succeed");

        assert_eq!(start_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn restart_should_wait_for_in_flight_start_for_same_chat() {
        let manager = Arc::new(runtime_manager());
        let first_started = Arc::new(Notify::new());
        let restart_entered = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());

        let first_manager = manager.clone();
        let first_marker = manager.clone();
        let first_started_signal = first_started.clone();
        let first_release = release_first.clone();
        let first = tokio::spawn(async move {
            first_manager
                .ensure_unregistered_chat_runtime(ChatId(1), async move {
                    first_started_signal.notify_one();
                    first_release.notified().await;
                    first_marker.registered.lock().await.insert(ChatId(1));
                    Ok(())
                })
                .await
        });

        first_started.notified().await;

        let restart_manager = manager.clone();
        let restart_entered_signal = restart_entered.clone();
        let restart = tokio::spawn(async move {
            restart_manager
                .restart_chat_runtime_for_test(ChatId(1), async move {
                    restart_entered_signal.notify_one();
                    Ok(())
                })
                .await
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(25), restart_entered.notified())
                .await
                .is_err(),
            "same-chat restart should wait behind the in-flight start"
        );

        release_first.notify_one();
        first
            .await
            .expect("first task should join")
            .expect("first cold start should succeed");
        restart
            .await
            .expect("restart task should join")
            .expect("restart should succeed");
    }

    #[tokio::test]
    async fn ensure_unregistered_chat_runtime_should_allow_different_chats_to_start_concurrently() {
        let manager = Arc::new(runtime_manager());
        let start_calls = Arc::new(AtomicUsize::new(0));
        let both_started = Arc::new(Barrier::new(2));

        let first_manager = manager.clone();
        let first_marker = manager.clone();
        let first_calls = start_calls.clone();
        let first_barrier = both_started.clone();
        let first = tokio::spawn(async move {
            first_manager
                .ensure_unregistered_chat_runtime(ChatId(1), async move {
                    first_calls.fetch_add(1, Ordering::SeqCst);
                    first_barrier.wait().await;
                    first_marker.registered.lock().await.insert(ChatId(1));
                    Ok(())
                })
                .await
        });

        let second_manager = manager.clone();
        let second_marker = manager.clone();
        let second_calls = start_calls.clone();
        let second_barrier = both_started.clone();
        let second = tokio::spawn(async move {
            second_manager
                .ensure_unregistered_chat_runtime(ChatId(2), async move {
                    second_calls.fetch_add(1, Ordering::SeqCst);
                    second_barrier.wait().await;
                    second_marker.registered.lock().await.insert(ChatId(2));
                    Ok(())
                })
                .await
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            first
                .await
                .expect("first task should join")
                .expect("first cold start should succeed");
            second
                .await
                .expect("second task should join")
                .expect("second cold start should succeed");
        })
        .await
        .expect("different chats should not block each other");

        assert_eq!(start_calls.load(Ordering::SeqCst), 2);
    }

    fn runtime_manager() -> RuntimeManager<FakeTelegram> {
        let (docker, codex, broker) = configs();
        let (_telegram, router) = fake_runtime_router();
        RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            Some(broker),
            Arc::new(router),
            PtyReadPolicy::default(),
        )
    }

    fn fake_runtime_router() -> (
        Arc<FakeTelegram>,
        crate::router::Router<ManagedCodexSession, FakeTelegram>,
    ) {
        (
            Arc::new(FakeTelegram),
            crate::router::Router::new(1, Arc::new(FakeTelegram)),
        )
    }

    struct FakeTelegram;

    #[async_trait]
    impl TelegramSink for FakeTelegram {
        async fn send_message(
            &self,
            _chat_id: ChatId,
            _text: &str,
        ) -> Result<(), crate::bot::telegram::TelegramError> {
            Ok(())
        }
    }
}
