use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexRequest {
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexTurn {
    pub output: String,
}

#[async_trait]
pub trait CodexSession: Send + Sync {
    async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError>;
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
