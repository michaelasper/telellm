use async_trait::async_trait;
use std::path::PathBuf;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexRequest {
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexTurn {
    pub output: String,
    pub generated_files: Vec<GeneratedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    pub workspace_path: String,
    pub host_path: PathBuf,
    pub file_name: String,
    pub bytes: u64,
}

impl CodexTurn {
    pub fn text(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            generated_files: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexTurnEvent {
    OutputSnapshot { output: String, is_final: bool },
}

pub type CodexEventSender = mpsc::Sender<CodexTurnEvent>;

#[async_trait]
pub trait CodexSession: Send + Sync {
    async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError>;

    async fn send_with_events(
        &self,
        request: CodexRequest,
        events: Option<CodexEventSender>,
    ) -> Result<CodexTurn, CodexSessionError> {
        let turn = self.send(request).await?;
        if let Some(events) = events
            && let Err(err) = events.try_send(CodexTurnEvent::OutputSnapshot {
                output: turn.output.clone(),
                is_final: true,
            })
        {
            tracing::debug!(error = %err, "dropped final Codex turn event");
        }
        Ok(turn)
    }

    async fn restart(&self) -> Result<(), CodexSessionError>;
}

#[derive(Debug, thiserror::Error)]
pub enum CodexSessionError {
    #[error("pty error: {0}")]
    Pty(String),
    #[error("process error: {0}")]
    Process(String),
    #[error("session closed")]
    Closed,
}
