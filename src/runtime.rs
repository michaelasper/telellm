use crate::{
    bot::telegram::TelegramSink,
    codex::pty::PtyCodexSession,
    config::{BrokerConfig, CodexConfig, DockerConfig},
    ids::ChatId,
    router::Router,
    sandbox::{SandboxBackend, SandboxError, SandboxSpec, docker::DockerSandboxBackend},
};
use async_trait::async_trait;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::Mutex;

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
}

pub struct RuntimeManager<T>
where
    T: TelegramSink + 'static,
{
    docker: DockerSandboxBackend,
    docker_config: DockerConfig,
    codex_config: CodexConfig,
    broker_config: BrokerConfig,
    router: Arc<Router<PtyCodexSession, T>>,
    registered: Mutex<HashSet<ChatId>>,
}

impl<T> RuntimeManager<T>
where
    T: TelegramSink + 'static,
{
    pub fn new(
        docker: DockerSandboxBackend,
        docker_config: DockerConfig,
        codex_config: CodexConfig,
        broker_config: BrokerConfig,
        router: Arc<Router<PtyCodexSession, T>>,
    ) -> Self {
        Self {
            docker,
            docker_config,
            codex_config,
            broker_config,
            router,
            registered: Mutex::new(HashSet::new()),
        }
    }

    fn spec_for_chat(&self, chat_id: ChatId) -> Result<SandboxSpec, RuntimeError> {
        let broker_url =
            reqwest::Url::parse(&self.broker_config.public_base_url).map_err(|_| {
                RuntimeError::InvalidBrokerUrl(self.broker_config.public_base_url.clone())
            })?;
        let broker_host = broker_url
            .host_str()
            .ok_or_else(|| {
                RuntimeError::InvalidBrokerUrl(self.broker_config.public_base_url.clone())
            })?
            .to_owned();
        let broker_port = broker_url
            .port_or_known_default()
            .unwrap_or_else(|| self.broker_config.listen.port());

        let mut spec = SandboxSpec::new(
            chat_id,
            &self.docker_config.image,
            &self.docker_config.network,
            &self.docker_config.workspace_volume_prefix,
        );
        spec.broker_host = broker_host;
        spec.broker_port = broker_port;
        spec.broker_base_url = self.broker_config.public_base_url.clone();
        Ok(spec)
    }

    async fn spawn_and_register_session(&self, spec: &SandboxSpec) -> Result<(), RuntimeError> {
        let mut codex_parts = Vec::with_capacity(self.codex_config.args.len() + 5);
        codex_parts.push(self.codex_config.command.as_str());
        codex_parts.extend(self.codex_config.args.iter().map(String::as_str));
        codex_parts.extend([
            "--model",
            self.codex_config.model.as_str(),
            "--cd",
            "/workspace",
        ]);

        let docker_args = DockerSandboxBackend::exec_args(spec, &codex_parts);
        let session = Arc::new(PtyCodexSession::spawn("docker", &docker_args)?);
        self.router.register_session(spec.chat_id, session).await;
        self.registered.lock().await.insert(spec.chat_id);
        Ok(())
    }

    async fn ensure_chat_runtime_inner(&self, chat_id: ChatId) -> Result<(), RuntimeError> {
        if self.registered.lock().await.contains(&chat_id) {
            return Ok(());
        }

        let spec = self.spec_for_chat(chat_id)?;
        self.docker.ensure_started(&spec).await?;
        self.spawn_and_register_session(&spec).await
    }

    async fn reset_chat_runtime_inner(
        &self,
        chat_id: ChatId,
        clear_workspace: bool,
    ) -> Result<(), RuntimeError> {
        let spec = self.spec_for_chat(chat_id)?;
        if clear_workspace {
            self.docker.rebuild(&spec, true).await?;
        } else {
            self.docker.ensure_started(&spec).await?;
        }
        self.spawn_and_register_session(&spec).await
    }

    async fn restart_chat_runtime_inner(&self, chat_id: ChatId) -> Result<(), RuntimeError> {
        let spec = self.spec_for_chat(chat_id)?;
        self.docker.restart(&spec).await?;
        self.spawn_and_register_session(&spec).await
    }

    async fn rebuild_chat_runtime_inner(
        &self,
        chat_id: ChatId,
        clear_workspace: bool,
    ) -> Result<(), RuntimeError> {
        let spec = self.spec_for_chat(chat_id)?;
        self.docker.rebuild(&spec, clear_workspace).await?;
        self.spawn_and_register_session(&spec).await
    }
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
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("sandbox error: {0}")]
    Sandbox(#[from] SandboxError),
    #[error("Codex session error: {0}")]
    Codex(#[from] crate::codex::session::CodexSessionError),
    #[error("broker public_base_url is invalid: {0}")]
    InvalidBrokerUrl(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BrokerConfig, CodexConfig, DockerConfig};
    use std::collections::BTreeMap;

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
            broker,
            Arc::new(router),
        );

        let spec = manager.spec_for_chat(ChatId(1)).expect("spec should build");

        assert_eq!(spec.broker_host, "host.docker.internal");
    }

    #[test]
    fn codex_exec_args_should_include_model_and_workspace() {
        let (docker, codex, broker) = configs();
        let (_telegram, router) = fake_runtime_router();
        let manager = RuntimeManager::new(
            DockerSandboxBackend,
            docker,
            codex,
            broker,
            Arc::new(router),
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

    fn fake_runtime_router() -> (
        Arc<FakeTelegram>,
        crate::router::Router<PtyCodexSession, FakeTelegram>,
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
