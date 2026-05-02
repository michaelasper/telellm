use crate::{
    bot::message::IncomingAttachment,
    bot::telegram::TelegramAudioSendKind,
    config::{AudioSendAs, AudioToolConfig, AudioTtsConfig},
    ids::{ChatId, MessageId},
};
use async_trait::async_trait;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

const DEFAULT_MAX_OUTPUT_BYTES: u64 = 20_000_000;
const STDERR_TAIL_BYTES: usize = 4096;
const STDERR_DRAIN_AFTER_REAP: Duration = Duration::from_millis(100);
const CHILD_REAP_AFTER_STDIN_ERROR: Duration = Duration::from_millis(100);
static TEMP_AUDIO_REPLY_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesizedAudio {
    pub host_path: PathBuf,
    pub file_name: String,
    pub send_as: AudioSendAs,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAudio {
    pub workspace_path: String,
    pub transcript: Option<String>,
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LocalStt {
    config: AudioToolConfig,
    max_output_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct LocalTts {
    config: AudioTtsConfig,
    max_output_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct AudioReplyRequest {
    pub chat_id: ChatId,
    pub trigger_message_id: MessageId,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("audio command `{command}` failed to spawn: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("audio command `{command}` stdin was unavailable after spawn")]
    StdinUnavailable { command: String },
    #[error("audio command `{command}` stdin write failed after spawn: {source}")]
    StdinWrite {
        command: String,
        source: std::io::Error,
    },
    #[error("audio command `{command}` wait failed after spawn: {source}")]
    Wait {
        command: String,
        source: std::io::Error,
    },
    #[error("audio command `{command}` timed out after {timeout_secs} seconds")]
    Timeout { command: String, timeout_secs: u64 },
    #[error("audio command `{command}` exited with status {status}: {stderr}")]
    Exit {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("audio command output `{path}` could not be read: {source}")]
    OutputRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("audio command produced empty output")]
    EmptyOutput,
    #[error("audio command output was {bytes} bytes, above the configured {limit} byte limit")]
    OutputTooLarge { bytes: u64, limit: u64 },
}

impl LocalStt {
    pub fn new(config: AudioToolConfig) -> Self {
        Self::new_with_max_output_bytes(config, DEFAULT_MAX_OUTPUT_BYTES)
    }

    pub fn new_with_max_output_bytes(config: AudioToolConfig, max_output_bytes: u64) -> Self {
        Self {
            config,
            max_output_bytes,
        }
    }

    pub async fn transcribe_to(
        &self,
        input: &Path,
        output: &Path,
    ) -> Result<Transcript, AudioError> {
        run_audio_command(
            &self.config.command,
            &expand_args(&self.config.args, Some(input), output),
            None,
            self.config.timeout_secs,
        )
        .await?;

        let bytes = output_len(output).await?;
        validate_nonempty_output(bytes)?;
        validate_output_limit(bytes, self.max_output_bytes)?;

        let text =
            tokio::fs::read_to_string(output)
                .await
                .map_err(|source| AudioError::OutputRead {
                    path: output.to_path_buf(),
                    source,
                })?;
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err(AudioError::EmptyOutput);
        }

        Ok(Transcript { text })
    }
}

impl LocalTts {
    pub fn new(config: AudioTtsConfig) -> Self {
        Self::new_with_max_output_bytes(config, DEFAULT_MAX_OUTPUT_BYTES)
    }

    pub fn new_with_max_output_bytes(config: AudioTtsConfig, max_output_bytes: u64) -> Self {
        Self {
            config,
            max_output_bytes,
        }
    }

    pub async fn synthesize_to(
        &self,
        text: &str,
        output: &Path,
    ) -> Result<SynthesizedAudio, AudioError> {
        let stdin_text = self.config.stdin_text.then_some(text);
        run_audio_command(
            &self.config.command,
            &expand_args(&self.config.args, None, output),
            stdin_text,
            self.config.timeout_secs,
        )
        .await?;

        let bytes = output_len(output).await?;
        validate_nonempty_output(bytes)?;
        validate_output_limit(bytes, self.max_output_bytes)?;

        Ok(SynthesizedAudio {
            host_path: output.to_path_buf(),
            file_name: output
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            send_as: self.config.send_as,
            bytes,
        })
    }
}

impl AudioSendAs {
    pub fn telegram_kind(self) -> TelegramAudioSendKind {
        match self {
            Self::Voice => TelegramAudioSendKind::Voice,
            Self::Audio => TelegramAudioSendKind::Audio,
        }
    }
}

pub fn is_audio_attachment(attachment: &IncomingAttachment) -> bool {
    attachment.kind.is_audio()
        || attachment
            .mime_type
            .as_deref()
            .is_some_and(|mime| mime.starts_with("audio/"))
}

pub fn audio_workspace_path(
    workspace_dir: &str,
    message_id: MessageId,
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
        crate::bot::message::sanitize_file_name(file_name)
    )
}

#[async_trait]
pub trait AudioReplySynthesizer: Send + Sync {
    async fn synthesize_reply(
        &self,
        request: AudioReplyRequest,
    ) -> Result<Option<SynthesizedAudio>, AudioError>;
}

#[async_trait]
impl AudioReplySynthesizer for LocalTts {
    async fn synthesize_reply(
        &self,
        request: AudioReplyRequest,
    ) -> Result<Option<SynthesizedAudio>, AudioError> {
        let output = temp_audio_reply_path(request.trigger_message_id, self.config.send_as);
        match self.synthesize_to(&request.text, &output).await {
            Ok(audio) => Ok(Some(audio)),
            Err(err) => {
                remove_temp_audio_reply(&output).await;
                Err(err)
            }
        }
    }
}

fn expand_args(args: &[String], input: Option<&Path>, output: &Path) -> Vec<String> {
    args.iter()
        .map(|arg| {
            let with_input = match input {
                Some(input) => arg.replace("{input}", &input.to_string_lossy()),
                None => arg.clone(),
            };
            with_input.replace("{output}", &output.to_string_lossy())
        })
        .collect()
}

async fn run_audio_command(
    command: &str,
    args: &[String],
    stdin_text: Option<&str>,
    timeout_secs: u64,
) -> Result<(), AudioError> {
    let mut process = Command::new(command);
    process
        .args(args)
        .kill_on_drop(true)
        .stdin(if stdin_text.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = process.spawn().map_err(|source| AudioError::Spawn {
        command: command.to_owned(),
        source,
    })?;
    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(read_stderr_tail(stderr));

    let command_result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        if let Some(text) = stdin_text {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| AudioError::StdinUnavailable {
                    command: command.to_owned(),
                })?;
            stdin
                .write_all(text.as_bytes())
                .await
                .map_err(|source| AudioError::StdinWrite {
                    command: command.to_owned(),
                    source,
                })?;
        }

        child.wait().await.map_err(|source| AudioError::Wait {
            command: command.to_owned(),
            source,
        })
    })
    .await;

    let status = match command_result {
        Ok(Ok(status)) => status,
        Ok(Err(err)) => {
            let status =
                match tokio::time::timeout(CHILD_REAP_AFTER_STDIN_ERROR, child.wait()).await {
                    Ok(status) => status,
                    Err(_) => {
                        let _ = child.kill().await;
                        child.wait().await
                    }
                };
            let stderr = collect_stderr_after_reap(stderr_task).await;
            match status {
                Ok(status) if !status.success() => {
                    return Err(AudioError::Exit {
                        command: command.to_owned(),
                        status: status.to_string(),
                        stderr,
                    });
                }
                Ok(_) | Err(_) => return Err(err),
            }
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = collect_stderr_after_reap(stderr_task).await;
            return Err(AudioError::Timeout {
                command: command.to_owned(),
                timeout_secs,
            });
        }
    };

    let stderr = collect_stderr_after_reap(stderr_task).await;

    if !status.success() {
        return Err(AudioError::Exit {
            command: command.to_owned(),
            status: status.to_string(),
            stderr,
        });
    }

    Ok(())
}

async fn output_len(path: &Path) -> Result<u64, AudioError> {
    tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.len())
        .map_err(|source| AudioError::OutputRead {
            path: path.to_path_buf(),
            source,
        })
}

fn validate_nonempty_output(bytes: u64) -> Result<(), AudioError> {
    if bytes == 0 {
        return Err(AudioError::EmptyOutput);
    }

    Ok(())
}

fn validate_output_limit(bytes: u64, limit: u64) -> Result<(), AudioError> {
    if bytes > limit {
        return Err(AudioError::OutputTooLarge { bytes, limit });
    }

    Ok(())
}

fn temp_audio_reply_path(message_id: MessageId, send_as: AudioSendAs) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = TEMP_AUDIO_REPLY_COUNTER.fetch_add(1, Ordering::Relaxed);
    let extension = match send_as {
        AudioSendAs::Voice => "ogg",
        AudioSendAs::Audio => "mp3",
    };
    std::env::temp_dir().join(format!(
        "telellm-tts-{}-{}-{nanos}-{counter}.{extension}",
        std::process::id(),
        message_id.0
    ))
}

async fn remove_temp_audio_reply(path: &Path) {
    if let Err(err) = tokio::fs::remove_file(path).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(
            error = %err,
            path = %path.display(),
            "failed to remove temporary spoken reply after synthesis failure"
        );
    }
}

async fn read_stderr_tail(stderr: Option<tokio::process::ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 1024];

    loop {
        let bytes_read = match stderr.read(&mut buffer).await {
            Ok(0) => break,
            Ok(bytes_read) => bytes_read,
            Err(_) => break,
        };
        tail.extend_from_slice(&buffer[..bytes_read]);
        if tail.len() > STDERR_TAIL_BYTES {
            let excess = tail.len() - STDERR_TAIL_BYTES;
            tail.drain(..excess);
        }
    }

    String::from_utf8_lossy(&tail).trim().to_owned()
}

async fn collect_stderr_after_reap(task: tokio::task::JoinHandle<String>) -> String {
    let mut task = task;
    match tokio::time::timeout(STDERR_DRAIN_AFTER_REAP, &mut task).await {
        Ok(Ok(stderr)) => stderr,
        Ok(Err(_)) => String::new(),
        Err(_) => {
            task.abort();
            let _ = task.await;
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudioSendAs, AudioToolConfig, AudioTtsConfig};
    use tempfile::tempdir;

    #[tokio::test]
    async fn stt_command_should_write_transcript() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("input.ogg");
        tokio::fs::write(&input, b"fake audio")
            .await
            .expect("write input");
        let output = dir.path().join("transcript.txt");
        let runner = LocalStt::new(AudioToolConfig {
            command: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "printf 'hello from audio' > \"$2\"".to_owned(),
                "test-sh".to_owned(),
                "{input}".to_owned(),
                "{output}".to_owned(),
            ],
            timeout_secs: 5,
        });

        let transcript = runner
            .transcribe_to(&input, &output)
            .await
            .expect("transcription should work");

        assert_eq!(transcript.text, "hello from audio");
    }

    #[tokio::test]
    async fn tts_command_should_write_audio_from_stdin() {
        let dir = tempdir().expect("tempdir");
        let output = dir.path().join("reply.ogg");
        let runner = LocalTts::new(AudioTtsConfig {
            command: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "cat > \"$1\"".to_owned(),
                "test-sh".to_owned(),
                "{output}".to_owned(),
            ],
            stdin_text: true,
            timeout_secs: 5,
            send_as: AudioSendAs::Voice,
        });

        let speech = runner
            .synthesize_to("spoken reply", &output)
            .await
            .expect("synthesis should work");

        assert_eq!(speech.host_path, output);
        assert_eq!(
            tokio::fs::read(&speech.host_path).await.expect("read"),
            b"spoken reply"
        );
        assert_eq!(speech.send_as, AudioSendAs::Voice);
    }

    #[tokio::test]
    async fn tts_stdin_write_should_obey_command_timeout() {
        let dir = tempdir().expect("tempdir");
        let output = dir.path().join("reply.ogg");
        let runner = LocalTts::new(AudioTtsConfig {
            command: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "sleep 5".to_owned(),
                "test-sh".to_owned(),
                "{output}".to_owned(),
            ],
            stdin_text: true,
            timeout_secs: 1,
            send_as: AudioSendAs::Voice,
        });
        let text = "x".repeat(1024 * 1024);

        let err = runner
            .synthesize_to(&text, &output)
            .await
            .expect_err("synthesis should time out");

        assert!(matches!(
            err,
            AudioError::Timeout {
                timeout_secs: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn tts_command_that_exits_before_reading_stdin_should_report_exit() {
        let dir = tempdir().expect("tempdir");
        let output = dir.path().join("reply.ogg");
        let runner = LocalTts::new(AudioTtsConfig {
            command: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "printf early-exit >&2; exit 7".to_owned(),
                "test-sh".to_owned(),
                "{output}".to_owned(),
            ],
            stdin_text: true,
            timeout_secs: 5,
            send_as: AudioSendAs::Voice,
        });
        let text = "x".repeat(4 * 1024 * 1024);

        let err = runner
            .synthesize_to(&text, &output)
            .await
            .expect_err("early command exit should fail");

        let AudioError::Exit { stderr, .. } = err else {
            panic!("expected exit error");
        };
        assert!(stderr.contains("early-exit"), "{stderr}");
    }

    #[tokio::test]
    async fn tts_stdin_write_failure_should_not_report_spawn_failure() {
        let dir = tempdir().expect("tempdir");
        let output = dir.path().join("reply.ogg");
        let runner = LocalTts::new(AudioTtsConfig {
            command: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "exit 0".to_owned(),
                "test-sh".to_owned(),
                "{output}".to_owned(),
            ],
            stdin_text: true,
            timeout_secs: 5,
            send_as: AudioSendAs::Voice,
        });
        let text = "x".repeat(4 * 1024 * 1024);

        let err = runner
            .synthesize_to(&text, &output)
            .await
            .expect_err("closed stdin should fail synthesis");

        let message = err.to_string();
        assert!(message.contains("stdin write failed"), "{message}");
        assert!(!message.contains("failed to spawn"), "{message}");
    }

    #[tokio::test]
    async fn stt_should_reject_oversized_transcript_before_reading_text() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("input.ogg");
        tokio::fs::write(&input, b"fake audio")
            .await
            .expect("write input");
        let output = dir.path().join("transcript.txt");
        let runner = LocalStt::new_with_max_output_bytes(
            AudioToolConfig {
                command: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "printf 'too large' > \"$2\"".to_owned(),
                    "test-sh".to_owned(),
                    "{input}".to_owned(),
                    "{output}".to_owned(),
                ],
                timeout_secs: 5,
            },
            3,
        );

        let err = runner
            .transcribe_to(&input, &output)
            .await
            .expect_err("oversized transcript should be rejected");

        assert!(matches!(
            err,
            AudioError::OutputTooLarge { bytes: 9, limit: 3 }
        ));
    }

    #[tokio::test]
    async fn tts_should_reject_oversized_audio_output() {
        let dir = tempdir().expect("tempdir");
        let output = dir.path().join("reply.ogg");
        let runner = LocalTts::new_with_max_output_bytes(
            AudioTtsConfig {
                command: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "printf 'audio bytes' > \"$1\"".to_owned(),
                    "test-sh".to_owned(),
                    "{output}".to_owned(),
                ],
                stdin_text: false,
                timeout_secs: 5,
                send_as: AudioSendAs::Voice,
            },
            5,
        );

        let err = runner
            .synthesize_to("spoken reply", &output)
            .await
            .expect_err("oversized audio should be rejected");

        assert!(matches!(
            err,
            AudioError::OutputTooLarge {
                bytes: 11,
                limit: 5
            }
        ));
    }

    #[tokio::test]
    async fn exit_error_should_keep_bounded_stderr_tail() {
        let dir = tempdir().expect("tempdir");
        let output = dir.path().join("reply.ogg");
        let runner = LocalTts::new(AudioTtsConfig {
            command: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "i=0; while [ \"$i\" -lt 9000 ]; do printf a >&2; i=$((i + 1)); done; printf tail-marker >&2; exit 7".to_owned(),
                "test-sh".to_owned(),
                "{output}".to_owned(),
            ],
            stdin_text: false,
            timeout_secs: 5,
            send_as: AudioSendAs::Voice,
        });

        let err = runner
            .synthesize_to("spoken reply", &output)
            .await
            .expect_err("nonzero command should fail");

        let AudioError::Exit { stderr, .. } = err else {
            panic!("expected exit error");
        };
        assert!(stderr.len() <= 4096);
        assert!(stderr.ends_with("tail-marker"));
    }

    #[tokio::test]
    async fn command_should_not_wait_for_descendant_inherited_stderr() {
        let dir = tempdir().expect("tempdir");
        let output = dir.path().join("reply.ogg");
        let runner = LocalTts::new(AudioTtsConfig {
            command: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "(sleep 2) & printf 'spoken reply' > \"$1\"".to_owned(),
                "test-sh".to_owned(),
                "{output}".to_owned(),
            ],
            stdin_text: false,
            timeout_secs: 5,
            send_as: AudioSendAs::Voice,
        });

        let speech = tokio::time::timeout(
            Duration::from_millis(500),
            runner.synthesize_to("spoken reply", &output),
        )
        .await
        .expect("adapter should not wait for descendant stderr")
        .expect("synthesis should work");

        assert_eq!(speech.bytes, 12);
    }

    #[test]
    fn audio_send_as_should_map_to_telegram_kind() {
        assert_eq!(
            AudioSendAs::Voice.telegram_kind(),
            TelegramAudioSendKind::Voice
        );
        assert_eq!(
            AudioSendAs::Audio.telegram_kind(),
            TelegramAudioSendKind::Audio
        );
    }
}
