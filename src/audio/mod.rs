use crate::{
    bot::telegram::TelegramAudioSendKind,
    config::{AudioSendAs, AudioToolConfig, AudioTtsConfig},
    ids::{ChatId, MessageId},
};
use async_trait::async_trait;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

const DEFAULT_MAX_OUTPUT_BYTES: u64 = 20_000_000;
const STDERR_TAIL_BYTES: usize = 4096;
const STDERR_DRAIN_AFTER_REAP: Duration = Duration::from_millis(100);

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

#[async_trait]
pub trait AudioReplySynthesizer: Send + Sync {
    async fn synthesize_reply(
        &self,
        request: AudioReplyRequest,
    ) -> Result<Option<SynthesizedAudio>, AudioError>;
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
            let mut stdin = child.stdin.take().ok_or_else(|| AudioError::Spawn {
                command: command.to_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "audio command stdin was unavailable",
                ),
            })?;
            stdin
                .write_all(text.as_bytes())
                .await
                .map_err(|source| AudioError::Spawn {
                    command: command.to_owned(),
                    source,
                })?;
        }

        child.wait().await.map_err(|source| AudioError::Spawn {
            command: command.to_owned(),
            source,
        })
    })
    .await;

    let status = match command_result {
        Ok(result) => result?,
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
