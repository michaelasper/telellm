use super::session::{CodexRequest, CodexSession, CodexSessionError, CodexTurn};
use async_trait::async_trait;
use portable_pty::{Child, CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone)]
pub struct PtyCodexSession {
    inner: Arc<Mutex<PtyInner>>,
}

struct PtyInner {
    _child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    reader: Box<dyn Read + Send>,
}

impl PtyCodexSession {
    pub fn spawn(command: &str, args: &[String]) -> Result<Self, CodexSessionError> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows: 30,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| CodexSessionError::Pty(err.to_string()))?;

        let mut cmd = CommandBuilder::new(command);
        for arg in args {
            cmd.arg(arg);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| CodexSessionError::Pty(err.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| CodexSessionError::Pty(err.to_string()))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| CodexSessionError::Pty(err.to_string()))?;

        Ok(Self {
            inner: Arc::new(Mutex::new(PtyInner {
                _child: child,
                writer,
                reader,
            })),
        })
    }
}

#[async_trait]
impl CodexSession for PtyCodexSession {
    async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner
                .lock()
                .map_err(|_| CodexSessionError::Pty("pty mutex poisoned".to_owned()))?;
            guard
                .writer
                .write_all(request.prompt.as_bytes())
                .map_err(|err| {
                    CodexSessionError::Pty(format!("failed writing prompt to pty: {err}"))
                })?;
            guard.writer.write_all(b"\n").map_err(|err| {
                CodexSessionError::Pty(format!("failed writing newline to pty: {err}"))
            })?;
            guard.writer.flush().map_err(|err| {
                CodexSessionError::Pty(format!("failed flushing pty writer: {err}"))
            })?;

            std::thread::sleep(Duration::from_millis(100));
            let mut buf = [0_u8; 4096];
            let bytes = guard
                .reader
                .read(&mut buf)
                .map_err(|err| CodexSessionError::Pty(format!("failed reading pty: {err}")))?;
            Ok(CodexTurn {
                output: String::from_utf8_lossy(&buf[..bytes]).into_owned(),
            })
        })
        .await
        .map_err(|err| CodexSessionError::Pty(err.to_string()))?
    }

    async fn restart(&self) -> Result<(), CodexSessionError> {
        Err(CodexSessionError::Pty(
            "restart requires the router to replace the PTY session".to_owned(),
        ))
    }
}
