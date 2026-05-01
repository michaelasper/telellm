use super::session::{CodexRequest, CodexSession, CodexSessionError, CodexTurn};
use async_trait::async_trait;
use portable_pty::{Child, CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::{
    io::{ErrorKind, Read, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use std::{sync::mpsc, thread};

const DEFAULT_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_MAX_TURN_TIMEOUT: Duration = Duration::from_secs(900);
const DEFAULT_MAX_OUTPUT_BYTES: usize = 512 * 1024;

#[derive(Clone)]
pub struct PtyCodexSession {
    inner: Arc<Mutex<PtyInner>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyReadPolicy {
    pub first_byte_timeout: Duration,
    pub inactivity_timeout: Duration,
    pub max_turn_timeout: Duration,
    pub max_output_bytes: usize,
}

impl Default for PtyReadPolicy {
    fn default() -> Self {
        Self {
            first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
            inactivity_timeout: DEFAULT_INACTIVITY_TIMEOUT,
            max_turn_timeout: DEFAULT_MAX_TURN_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

struct PtyInner {
    _child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output_rx: mpsc::Receiver<Vec<u8>>,
    read_policy: PtyReadPolicy,
}

impl PtyCodexSession {
    pub fn spawn(command: &str, args: &[String]) -> Result<Self, CodexSessionError> {
        Self::spawn_with_read_policy(command, args, PtyReadPolicy::default())
    }

    pub fn spawn_with_read_policy(
        command: &str,
        args: &[String],
        read_policy: PtyReadPolicy,
    ) -> Result<Self, CodexSessionError> {
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
        let output_rx = spawn_reader_thread(reader);

        Ok(Self {
            inner: Arc::new(Mutex::new(PtyInner {
                _child: child,
                writer,
                output_rx,
                read_policy,
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
            guard.drain_stale_output();
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

            Ok(CodexTurn {
                output: guard.read_turn_output(&request.prompt)?,
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

impl PtyInner {
    fn drain_stale_output(&mut self) {
        while self.output_rx.try_recv().is_ok() {}
    }

    fn read_turn_output(&mut self, prompt: &str) -> Result<String, CodexSessionError> {
        let deadline = Instant::now() + self.read_policy.max_turn_timeout;
        let mut output = Vec::new();

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(CodexSessionError::Pty(
                    "timed out waiting for Codex turn to become idle".to_owned(),
                ));
            }

            let idle_timeout = if output.is_empty() {
                self.read_policy.first_byte_timeout
            } else {
                self.read_policy.inactivity_timeout
            };
            let timeout = idle_timeout.min(deadline.saturating_duration_since(now));

            match self.output_rx.recv_timeout(timeout) {
                Ok(chunk) => {
                    output.extend_from_slice(&chunk);
                    if output.len() > self.read_policy.max_output_bytes {
                        return Err(CodexSessionError::Pty(format!(
                            "Codex turn exceeded {} bytes of PTY output",
                            self.read_policy.max_output_bytes
                        )));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) if output.is_empty() => {
                    return Err(CodexSessionError::Pty(
                        "timed out waiting for Codex output".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let cleaned = clean_pty_output(prompt, &output);
                    if turn_output_is_incomplete(prompt, &cleaned) {
                        continue;
                    }
                    return Ok(cleaned);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) if output.is_empty() => {
                    return Err(CodexSessionError::Closed);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let cleaned = clean_pty_output(prompt, &output);
                    if turn_output_is_incomplete(prompt, &cleaned) {
                        return Err(CodexSessionError::Closed);
                    }
                    return Ok(cleaned);
                }
            }
        }
    }
}

fn spawn_reader_thread(mut reader: Box<dyn Read + Send>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(bytes) => {
                    if tx.send(buf[..bytes].to_vec()).is_err() {
                        break;
                    }
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    rx
}

fn clean_pty_output(prompt: &str, output: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(output);
    let normalized = normalize_newlines(&decoded);
    let without_ansi = strip_ansi_sequences(&normalized);
    strip_prompt_echo(prompt, &without_ansi)
        .trim_matches('\n')
        .to_owned()
}

fn turn_output_is_incomplete(prompt: &str, output: &str) -> bool {
    let trimmed = output.trim();
    trimmed.is_empty()
        || output_is_only_prompt_echo(prompt, trimmed)
        || output_is_interactive_codex_prompt(trimmed)
}

fn output_is_only_prompt_echo(prompt: &str, output: &str) -> bool {
    let compact_prompt = compact_terminal_text(prompt);
    let compact_output = compact_terminal_text(output);

    !compact_prompt.is_empty()
        && !compact_output.is_empty()
        && (compact_prompt == compact_output || compact_prompt.starts_with(&compact_output))
}

fn output_is_interactive_codex_prompt(output: &str) -> bool {
    let compact_output = compact_terminal_text(output);
    compact_output.contains("doyoutrustthecontentsofthisdirectory")
        || compact_output.contains("pressentertocontinue")
}

fn compact_terminal_text(input: &str) -> String {
    input
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalize_newlines(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}

fn strip_prompt_echo(prompt: &str, output: &str) -> String {
    let normalized_prompt = normalize_newlines(prompt);
    let prompt_lines: Vec<&str> = normalized_prompt.trim_matches('\n').lines().collect();
    if prompt_lines.is_empty() {
        return output.to_owned();
    }

    let mut output_lines = output.lines();
    let mut remaining_prompt = prompt_lines.as_slice();
    while let Some(prompt_line) = remaining_prompt.first() {
        match output_lines.next() {
            Some(output_line) if output_line.trim_end() == prompt_line.trim_end() => {
                remaining_prompt = &remaining_prompt[1..];
            }
            Some(output_line) => {
                let mut rebuilt = String::from(output_line);
                for line in output_lines {
                    rebuilt.push('\n');
                    rebuilt.push_str(line);
                }
                return rebuilt;
            }
            None => return String::new(),
        }
    }

    output_lines.collect::<Vec<_>>().join("\n")
}

fn strip_ansi_sequences(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    let mut previous_escape = false;
                    for c in chars.by_ref() {
                        if c == '\u{7}' || (previous_escape && c == '\\') {
                            break;
                        }
                        previous_escape = c == '\u{1b}';
                    }
                }
                Some(_) | None => {}
            }
            continue;
        }

        if ch == '\n' || ch == '\t' || !ch.is_control() {
            output.push(ch);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_pty_output_should_strip_prompt_echo_and_ansi_control_sequences() {
        let raw = b"\x1b[?25lhello\r\n\x1b[32manswer\x1b[0m\r\n";

        let cleaned = clean_pty_output("hello", raw);

        assert_eq!(cleaned, "answer");
    }

    #[test]
    fn turn_output_is_incomplete_should_detect_prompt_echo() {
        let prompt = "Line one\nLine two";

        assert!(turn_output_is_incomplete(prompt, "Line one\nLine two"));
    }

    #[test]
    fn turn_output_is_incomplete_should_detect_partial_prompt_echo() {
        let prompt = "Line one\nLine two";

        assert!(turn_output_is_incomplete(prompt, "Line one"));
    }

    #[test]
    fn turn_output_is_incomplete_should_detect_codex_trust_prompt() {
        let output = "Do you trust the contents of this directory?\n1. Yes, continue\nPress enter to continue";

        assert!(turn_output_is_incomplete("hello", output));
    }
}
