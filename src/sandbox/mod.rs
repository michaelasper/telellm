pub mod docker;
pub mod network;

use crate::{
    broker::BrokerToken,
    ids::{ChatId, SandboxId},
};
use async_trait::async_trait;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    pub chat_id: ChatId,
    pub sandbox_id: SandboxId,
    pub image: String,
    pub network: String,
    pub workspace_volume: String,
    pub broker_host: String,
    pub broker_port: u16,
    pub broker_base_url: String,
    pub broker_token: BrokerToken,
    pub codex_auth_host_path: Option<PathBuf>,
}

impl SandboxSpec {
    pub fn new(chat_id: ChatId, image: &str, network: &str, volume_prefix: &str) -> Self {
        let sandbox_id = SandboxId::for_chat(chat_id);
        Self {
            chat_id,
            workspace_volume: format!("{volume_prefix}_{}", sandbox_id.as_str()),
            sandbox_id,
            image: image.to_owned(),
            network: network.to_owned(),
            broker_host: "host.docker.internal".to_owned(),
            broker_port: 8189,
            broker_base_url: "http://host.docker.internal:8189/v1".to_owned(),
            broker_token: BrokerToken::generate(),
            codex_auth_host_path: None,
        }
    }
}

#[async_trait]
pub trait SandboxBackend: Send + Sync {
    async fn ensure_started(&self, spec: &SandboxSpec) -> Result<(), SandboxError>;
    async fn restart(&self, spec: &SandboxSpec) -> Result<(), SandboxError>;
    async fn rebuild(&self, spec: &SandboxSpec, clear_workspace: bool) -> Result<(), SandboxError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("docker command failed: {0}")]
    Docker(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Codex auth host path `{}` is invalid: {reason}", path.display())]
    InvalidCodexAuthPath { path: PathBuf, reason: String },
    #[error("workspace path `{path}` is invalid: {reason}")]
    InvalidWorkspacePath { path: String, reason: String },
    #[error("workspace file `{path}` is too large: {bytes} bytes exceeds limit {max_bytes}")]
    WorkspaceFileTooLarge {
        path: String,
        bytes: u64,
        max_bytes: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_spec_new_should_create_group_scoped_volume() {
        let spec = SandboxSpec::new(ChatId(-100), "img", "net", "vol");

        assert_eq!(spec.workspace_volume, "vol_telellm-chat-neg100");
    }
}
