use super::session::{CodexRequest, CodexSession, CodexSessionError, CodexTurn};
use async_trait::async_trait;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

#[derive(Debug, Clone)]
pub struct CommandCodexSession {
    command: String,
    args: Vec<String>,
    timeout: Duration,
    max_output_bytes: usize,
}

impl CommandCodexSession {
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            timeout,
            max_output_bytes,
        }
    }
}

#[async_trait]
impl CodexSession for CommandCodexSession {
    async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
        let mut child = Command::new(&self.command)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                CodexSessionError::Process(format!("failed to spawn Codex command: {err}"))
            })?;

        let mut stdin = child.stdin.take().ok_or_else(|| {
            CodexSessionError::Process("Codex command stdin was unavailable".to_owned())
        })?;
        stdin
            .write_all(request.prompt.as_bytes())
            .await
            .map_err(|err| {
                CodexSessionError::Process(format!("failed writing prompt to Codex command: {err}"))
            })?;
        stdin.write_all(b"\n").await.map_err(|err| {
            CodexSessionError::Process(format!("failed writing newline to Codex command: {err}"))
        })?;
        stdin.shutdown().await.map_err(|err| {
            CodexSessionError::Process(format!("failed closing Codex command stdin: {err}"))
        })?;
        drop(stdin);

        let stdout = child.stdout.take().ok_or_else(|| {
            CodexSessionError::Process("Codex command stdout was unavailable".to_owned())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            CodexSessionError::Process("Codex command stderr was unavailable".to_owned())
        })?;
        let max_output_bytes = self.max_output_bytes;
        let stdout_task = tokio::spawn(read_limited(stdout, max_output_bytes));
        let stderr_task = tokio::spawn(read_limited(stderr, max_output_bytes));

        let status = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(err)) => {
                return Err(CodexSessionError::Process(format!(
                    "failed waiting for Codex command: {err}"
                )));
            }
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                return Err(CodexSessionError::Process(
                    "timed out waiting for Codex command".to_owned(),
                ));
            }
        };

        let stdout = join_reader(stdout_task).await?;
        let stderr = join_reader(stderr_task).await?;
        let stderr_text = String::from_utf8_lossy(&stderr);

        if !status.success() {
            return Err(CodexSessionError::Process(format!(
                "Codex command exited with {status}: {}",
                stderr_text.trim()
            )));
        }

        Ok(CodexTurn::text(
            String::from_utf8_lossy(&stdout).trim().to_owned(),
        ))
    }

    async fn restart(&self) -> Result<(), CodexSessionError> {
        Ok(())
    }
}

async fn read_limited<R>(
    mut reader: R,
    max_output_bytes: usize,
) -> Result<Vec<u8>, CodexSessionError>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let bytes = reader.read(&mut buffer).await.map_err(|err| {
            CodexSessionError::Process(format!("failed reading Codex command output: {err}"))
        })?;
        if bytes == 0 {
            return Ok(output);
        }
        output.extend_from_slice(&buffer[..bytes]);
        if output.len() > max_output_bytes {
            return Err(CodexSessionError::Process(format!(
                "Codex command exceeded {max_output_bytes} bytes of output"
            )));
        }
    }
}

async fn join_reader(
    task: tokio::task::JoinHandle<Result<Vec<u8>, CodexSessionError>>,
) -> Result<Vec<u8>, CodexSessionError> {
    task.await
        .map_err(|err| CodexSessionError::Process(err.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_session_should_send_prompt_to_stdin_and_return_stdout() {
        let session = CommandCodexSession::new(
            "sh",
            vec![
                "-lc".to_owned(),
                "while IFS= read -r line; do printf 'answer:%s\\n' \"$line\"; done".to_owned(),
            ],
            Duration::from_secs(2),
            1024,
        );

        let turn = session
            .send(CodexRequest {
                prompt: "hello".to_owned(),
            })
            .await
            .expect("command should succeed");

        assert_eq!(turn.output, "answer:hello");
    }

    #[tokio::test]
    async fn command_session_should_report_nonzero_exit() {
        let session = CommandCodexSession::new(
            "sh",
            vec![
                "-lc".to_owned(),
                "cat >/dev/null; printf 'bad\\n' >&2; exit 7".to_owned(),
            ],
            Duration::from_secs(2),
            1024,
        );

        let err = session
            .send(CodexRequest {
                prompt: "hello".to_owned(),
            })
            .await
            .expect_err("command should fail");

        assert!(err.to_string().contains("bad"));
    }
}
