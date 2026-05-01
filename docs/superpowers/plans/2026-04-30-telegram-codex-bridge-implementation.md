# Telegram Codex Bridge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the first usable Rust daemon that connects Telegram group chats to persistent, isolated Codex CLI sandboxes with group memory and safe host boundaries.

**Architecture:** The daemon owns Telegram I/O, routing, durable memory, sandbox lifecycle, and the host auth broker. Each Telegram group gets one Docker/Colima sandbox, one persistent workspace volume, and one supervised long-lived Codex CLI session. The sandbox can reach public internet and the broker endpoint only; it cannot reach arbitrary host or LAN services.

**Tech Stack:** Rust 2024, Tokio, teloxide, SQLx with SQLite, portable-pty, axum, reqwest, Docker CLI, Codex CLI, tracing, thiserror, anyhow at the binary boundary.

---

## Scope Check

This plan implements the approved MVP as one integrated project because the Telegram adapter, router, memory context, sandbox supervisor, and Codex PTY are coupled in the first working loop. Each task produces a testable slice and ends with a commit. Docker/Codex network enforcement is introduced behind command builders and integration tests before the live Telegram loop is enabled.

## Reference Notes

- Local `codex --help` shows interactive CLI, `exec`, `resume`, `--cd`, `--sandbox`, `--ask-for-approval`, and `-c key=value` configuration overrides.
- Official Codex install references support `npm install -g @openai/codex` and Linux release binaries for container use: https://help.openai.com/en/articles/11096431-openai-codex-cli-getting-started and https://github.com/openai/codex.
- Official Codex configuration reference documents `config.toml`, providers, `CODEX_HOME`, and config overrides: https://developers.openai.com/codex/config-reference.

## File Structure

Create these files during the plan:

- `Cargo.toml`: package metadata, dependencies, lint settings.
- `rust-toolchain.toml`: pin the Rust channel used by this repo.
- `config.example.toml`: documented local configuration with no secrets.
- `README.md`: local setup, sandbox model, and run commands.
- `scripts/check.sh`: local validation command.
- `Dockerfile.sandbox`: Linux sandbox image with Codex CLI and workspace tooling.
- `.dockerignore`: keep build context small.
- `src/lib.rs`: module exports.
- `src/main.rs`: binary entry point and top-level error reporting.
- `src/app.rs`: app assembly, startup, shutdown, and long-running tasks.
- `src/runtime.rs`: per-chat sandbox and Codex session creation.
- `src/ids.rs`: typed IDs for Telegram and sandbox resources.
- `src/config.rs`: config loading, validation, and secret source modeling.
- `src/bot/mod.rs`: Telegram-facing module exports.
- `src/bot/command.rs`: slash command parser.
- `src/bot/message.rs`: normalized message model and addressing rules.
- `src/bot/chunk.rs`: Telegram message chunking.
- `src/bot/telegram.rs`: teloxide polling and sending adapter.
- `src/router.rs`: per-group queues, serialization, and request dispatch.
- `src/memory/mod.rs`: memory module exports and shared types.
- `src/memory/rolling.rs`: short raw catch-up buffer.
- `src/memory/store.rs`: `MemoryStore` trait and durable memory types.
- `src/memory/sqlite.rs`: SQLite-backed store.
- `src/memory/context.rs`: prompt context packet construction.
- `src/sandbox/mod.rs`: sandbox module exports and trait.
- `src/sandbox/docker.rs`: Docker CLI backend and lifecycle commands.
- `src/sandbox/network.rs`: network policy data and generated enforcement script.
- `src/codex/mod.rs`: Codex module exports.
- `src/codex/session.rs`: session trait and request/response types.
- `src/codex/pty.rs`: PTY-backed long-lived process supervisor.
- `src/codex/prompt.rs`: prompt envelope formatting.
- `src/broker.rs`: host-managed OpenAI-compatible auth broker.
- `tests/docker_sandbox.rs`: ignored integration tests requiring Docker.
- `tests/pty_session.rs`: PTY integration tests.

## Validation Commands

Run these after every task that touches Rust code:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Use this full script once `scripts/check.sh` exists:

```bash
./scripts/check.sh
```

---

### Task 1: Scaffold The Rust Package And Quality Gates

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `scripts/check.sh`

- [ ] **Step 1: Initialize the package**

Run:

```bash
cargo init --bin --name telellm .
```

Expected: `Cargo.toml` and `src/main.rs` exist.

- [ ] **Step 2: Add dependencies**

Run these exact commands:

```bash
cargo add anyhow
cargo add async-trait
cargo add axum
cargo add clap --features derive
cargo add portable-pty
cargo add reqwest --features json,rustls-tls
cargo add secrecy --features serde
cargo add serde --features derive
cargo add serde_json
cargo add sqlx --features runtime-tokio-rustls,sqlite
cargo add teloxide --features macros
cargo add thiserror
cargo add tokio --features full
cargo add toml
cargo add tracing
cargo add tracing-subscriber --features env-filter,json
cargo add tower --dev --features util
cargo add tempfile --dev
```

Expected: `Cargo.toml` includes the added crates and `Cargo.lock` is created.

- [ ] **Step 3: Add lint policy to `Cargo.toml`**

Append this content:

```toml
[lints.rust]
future_incompatible = "warn"
nonstandard_style = "deny"

[lints.clippy]
all = { level = "deny", priority = 10 }
redundant_clone = { level = "deny", priority = 9 }
clone_on_copy = { level = "deny", priority = 9 }
large_enum_variant = { level = "deny", priority = 8 }
needless_collect = { level = "deny", priority = 8 }
manual_ok_or = { level = "deny", priority = 8 }
```

- [ ] **Step 4: Pin the Rust toolchain**

Create `rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]
```

- [ ] **Step 5: Create the library module shell**

Replace `src/lib.rs` with:

```rust
pub mod app;
pub mod bot;
pub mod broker;
pub mod codex;
pub mod config;
pub mod ids;
pub mod memory;
pub mod router;
pub mod runtime;
pub mod sandbox;
```

- [ ] **Step 6: Create the binary entry point**

Replace `src/main.rs` with:

```rust
use anyhow::Context;
use clap::Parser;
use telellm::{app, config::AppConfig};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "telellm")]
#[command(about = "Telegram group chat bridge for sandboxed Codex CLI sessions")]
struct Cli {
    #[arg(long, default_value = "config.toml")]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let config = AppConfig::from_path(&cli.config)
        .with_context(|| format!("failed to load config from {}", cli.config.display()))?;

    app::run(config).await
}
```

- [ ] **Step 7: Create temporary module stubs so the package compiles**

Create these files with the shown content:

```rust
// src/app.rs
use crate::config::AppConfig;

pub async fn run(_config: AppConfig) -> anyhow::Result<()> {
    Ok(())
}
```

```rust
// src/bot/mod.rs
pub mod chunk;
pub mod command;
pub mod message;
pub mod telegram;
```

```rust
// src/bot/chunk.rs
pub fn chunk_for_telegram(input: &str, limit: usize) -> Vec<String> {
    if input.is_empty() {
        return Vec::new();
    }
    input.as_bytes()
        .chunks(limit)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect()
}
```

```rust
// src/bot/command.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommand {
    Help,
}
```

```rust
// src/bot/message.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addressing {
    Ambient,
    Addressed,
}
```

```rust
// src/bot/telegram.rs
pub struct TelegramAdapter;
```

```rust
// src/broker.rs
pub struct BrokerConfig;
```

```rust
// src/codex/mod.rs
pub mod prompt;
pub mod pty;
pub mod session;
```

```rust
// src/codex/prompt.rs
pub struct PromptEnvelope;
```

```rust
// src/codex/pty.rs
pub struct PtyCodexSession;
```

```rust
// src/codex/session.rs
pub struct CodexTurn;
```

```rust
// src/config.rs
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig;

impl AppConfig {
    pub fn from_path(_path: &std::path::Path) -> Result<Self, ConfigError> {
        Ok(Self)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid config")]
    Invalid,
}
```

```rust
// src/ids.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChatId(pub i64);
```

```rust
// src/memory/mod.rs
pub mod context;
pub mod rolling;
pub mod sqlite;
pub mod store;
```

```rust
// src/memory/context.rs
pub struct ContextPacket;
```

```rust
// src/memory/rolling.rs
pub struct RollingBuffer;
```

```rust
// src/memory/sqlite.rs
pub struct SqliteMemoryStore;
```

```rust
// src/memory/store.rs
pub struct MemoryRecord;
```

```rust
// src/router.rs
pub struct Router;
```

```rust
// src/runtime.rs
pub struct RuntimeManager;
```

```rust
// src/sandbox/mod.rs
pub mod docker;
pub mod network;
```

```rust
// src/sandbox/docker.rs
pub struct DockerSandboxBackend;
```

```rust
// src/sandbox/network.rs
pub struct NetworkPolicy;
```

- [ ] **Step 8: Add the check script**

Create `scripts/check.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Run:

```bash
chmod +x scripts/check.sh
./scripts/check.sh
```

Expected: all commands pass.

- [ ] **Step 9: Commit**

Run:

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml src scripts/check.sh
git commit -m "chore: scaffold Rust daemon"
```

---

### Task 2: Add Typed IDs And Config Validation

**Files:**
- Modify: `src/ids.rs`
- Modify: `src/config.rs`
- Create: `config.example.toml`

- [ ] **Step 1: Write typed ID tests in `src/ids.rs`**

Replace `src/ids.rs` with:

```rust
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub i32);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SandboxId(String);

impl SandboxId {
    pub fn for_chat(chat_id: ChatId) -> Self {
        let raw = chat_id.0.to_string().replace('-', "neg");
        Self(format!("telellm-chat-{raw}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SandboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_id_for_chat_should_be_stable_for_positive_chat_ids() {
        let sandbox_id = SandboxId::for_chat(ChatId(12345));

        assert_eq!(sandbox_id.as_str(), "telellm-chat-12345");
    }

    #[test]
    fn sandbox_id_for_chat_should_not_include_minus_sign_for_negative_chat_ids() {
        let sandbox_id = SandboxId::for_chat(ChatId(-10012345));

        assert_eq!(sandbox_id.as_str(), "telellm-chat-neg10012345");
    }
}
```

- [ ] **Step 2: Run typed ID tests**

Run:

```bash
cargo test ids::
```

Expected: PASS.

- [ ] **Step 3: Replace `src/config.rs` with validated config types**

Use this content:

```rust
use secrecy::SecretString;
use serde::Deserialize;
use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub telegram: TelegramConfig,
    pub storage: StorageConfig,
    pub docker: DockerConfig,
    pub codex: CodexConfig,
    pub broker: BrokerConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub bot_token_env: String,
    pub bot_username: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    pub sqlite_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DockerConfig {
    pub image: String,
    pub network: String,
    pub workspace_volume_prefix: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodexConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub model: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BrokerConfig {
    pub listen: SocketAddr,
    pub public_base_url: String,
    pub upstream_base_url: String,
    pub upstream_api_key_env: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    #[serde(default = "default_queue_depth")]
    pub per_group_queue_depth: usize,
    #[serde(default = "default_telegram_chunk_chars")]
    pub telegram_chunk_chars: usize,
    #[serde(default = "default_recent_buffer_messages")]
    pub recent_buffer_messages: usize,
    #[serde(default = "default_codex_inactivity_secs")]
    pub codex_inactivity_secs: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            per_group_queue_depth: default_queue_depth(),
            telegram_chunk_chars: default_telegram_chunk_chars(),
            recent_buffer_messages: default_recent_buffer_messages(),
            codex_inactivity_secs: default_codex_inactivity_secs(),
        }
    }
}

impl AppConfig {
    pub fn from_path(path: &std::path::Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_toml_str(&raw)
    }

    pub fn from_toml_str(raw: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(raw)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        require_non_empty("telegram.bot_token_env", &self.telegram.bot_token_env)?;
        require_non_empty("telegram.bot_username", &self.telegram.bot_username)?;
        require_non_empty("docker.image", &self.docker.image)?;
        require_non_empty("docker.network", &self.docker.network)?;
        require_non_empty("docker.workspace_volume_prefix", &self.docker.workspace_volume_prefix)?;
        require_non_empty("codex.command", &self.codex.command)?;
        require_non_empty("codex.model", &self.codex.model)?;
        require_non_empty("broker.public_base_url", &self.broker.public_base_url)?;
        require_non_empty("broker.upstream_base_url", &self.broker.upstream_base_url)?;
        require_non_empty("broker.upstream_api_key_env", &self.broker.upstream_api_key_env)?;

        if self.limits.per_group_queue_depth == 0 {
            return Err(ConfigError::InvalidValue {
                field: "limits.per_group_queue_depth",
                reason: "must be greater than zero",
            });
        }

        if self.limits.telegram_chunk_chars < 256 {
            return Err(ConfigError::InvalidValue {
                field: "limits.telegram_chunk_chars",
                reason: "must be at least 256",
            });
        }

        Ok(())
    }

    pub fn telegram_token_from_env(&self) -> Result<SecretString, ConfigError> {
        let value = std::env::var(&self.telegram.bot_token_env).map_err(|_| {
            ConfigError::MissingSecretEnv {
                env: self.telegram.bot_token_env.clone(),
            }
        })?;
        Ok(SecretString::from(value))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse config TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("config field `{field}` is invalid: {reason}")]
    InvalidValue {
        field: &'static str,
        reason: &'static str,
    },
    #[error("required secret environment variable `{env}` is not set")]
    MissingSecretEnv { env: String },
}

fn require_non_empty(field: &'static str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::InvalidValue {
            field,
            reason: "must not be empty",
        });
    }
    Ok(())
}

pub fn codex_inactivity_timeout(config: &AppConfig) -> Duration {
    Duration::from_secs(config.limits.codex_inactivity_secs)
}

fn default_queue_depth() -> usize {
    16
}

fn default_telegram_chunk_chars() -> usize {
    3900
}

fn default_recent_buffer_messages() -> usize {
    200
}

fn default_codex_inactivity_secs() -> u64 {
    600
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> &'static str {
        r#"
            [telegram]
            bot_token_env = "TELEGRAM_BOT_TOKEN"
            bot_username = "telellm_bot"

            [storage]
            sqlite_path = "data/telellm.sqlite"

            [docker]
            image = "telellm-sandbox:local"
            network = "telellm_public"
            workspace_volume_prefix = "telellm_workspace"

            [codex]
            command = "codex"
            args = ["--sandbox", "danger-full-access", "--ask-for-approval", "never"]
            model = "gpt-5-codex"

            [broker]
            listen = "127.0.0.1:8189"
            public_base_url = "http://host.docker.internal:8189/v1"
            upstream_base_url = "https://api.openai.com/v1"
            upstream_api_key_env = "OPENAI_API_KEY"
        "#
    }

    #[test]
    fn from_toml_str_should_parse_valid_config() {
        let config = AppConfig::from_toml_str(valid_config()).expect("config should parse");

        assert_eq!(config.telegram.bot_username, "telellm_bot");
    }

    #[test]
    fn from_toml_str_should_reject_empty_bot_username() {
        let raw = valid_config().replace("telellm_bot", "");

        let err = AppConfig::from_toml_str(&raw).expect_err("config should be invalid");

        assert_eq!(
            err.to_string(),
            "config field `telegram.bot_username` is invalid: must not be empty"
        );
    }

    #[test]
    fn from_toml_str_should_apply_default_limits() {
        let config = AppConfig::from_toml_str(valid_config()).expect("config should parse");

        assert_eq!(config.limits.per_group_queue_depth, 16);
    }
}
```

- [ ] **Step 4: Add example config**

Create `config.example.toml`:

```toml
[telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"
bot_username = "telellm_bot"

[storage]
sqlite_path = "data/telellm.sqlite"

[docker]
image = "telellm-sandbox:local"
network = "telellm_public"
workspace_volume_prefix = "telellm_workspace"

[codex]
command = "codex"
args = ["--sandbox", "danger-full-access", "--ask-for-approval", "never", "--no-alt-screen"]
model = "gpt-5-codex"

[broker]
listen = "127.0.0.1:8189"
public_base_url = "http://host.docker.internal:8189/v1"
upstream_base_url = "https://api.openai.com/v1"
upstream_api_key_env = "OPENAI_API_KEY"

[limits]
per_group_queue_depth = 16
telegram_chunk_chars = 3900
recent_buffer_messages = 200
codex_inactivity_secs = 600
```

- [ ] **Step 5: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 6: Commit**

Run:

```bash
git add src/ids.rs src/config.rs config.example.toml
git commit -m "feat: add typed ids and config validation"
```

---

### Task 3: Implement Bot Commands, Addressing, And Chunking

**Files:**
- Modify: `src/bot/command.rs`
- Modify: `src/bot/message.rs`
- Modify: `src/bot/chunk.rs`

- [ ] **Step 1: Replace command parser with tests**

Use this content for `src/bot/command.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommand {
    Help,
    Status,
    Reset { clear_workspace: bool },
    Restart,
    Rebuild { clear_workspace: bool },
    Memory,
    Forget { target: ForgetTarget },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetTarget {
    All,
    Query(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommandParseError {
    #[error("unknown command `{0}`")]
    Unknown(String),
    #[error("missing argument for `{0}`")]
    MissingArgument(&'static str),
}

impl BotCommand {
    pub fn parse(input: &str, bot_username: &str) -> Result<Option<Self>, CommandParseError> {
        let trimmed = input.trim();
        let Some(first) = trimmed.split_whitespace().next() else {
            return Ok(None);
        };
        if !first.starts_with('/') {
            return Ok(None);
        }

        let command_name = first
            .trim_start_matches('/')
            .split('@')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();

        if let Some((_, addressed_to)) = first.split_once('@') {
            if !addressed_to.eq_ignore_ascii_case(bot_username) {
                return Ok(None);
            }
        }

        let rest = trimmed[first.len()..].trim();
        match command_name.as_str() {
            "help" => Ok(Some(Self::Help)),
            "status" => Ok(Some(Self::Status)),
            "reset" => Ok(Some(Self::Reset {
                clear_workspace: rest.contains("--clear-workspace"),
            })),
            "restart" => Ok(Some(Self::Restart)),
            "rebuild" => Ok(Some(Self::Rebuild {
                clear_workspace: rest.contains("--clear-workspace"),
            })),
            "memory" => Ok(Some(Self::Memory)),
            "forget" => {
                if rest.is_empty() {
                    return Err(CommandParseError::MissingArgument("/forget"));
                }
                if rest == "all" {
                    Ok(Some(Self::Forget {
                        target: ForgetTarget::All,
                    }))
                } else {
                    Ok(Some(Self::Forget {
                        target: ForgetTarget::Query(rest.to_owned()),
                    }))
                }
            }
            other => Err(CommandParseError::Unknown(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_should_return_none_for_plain_text() {
        let parsed = BotCommand::parse("hello", "telellm_bot").expect("parse should succeed");

        assert_eq!(parsed, None);
    }

    #[test]
    fn parse_should_accept_command_addressed_to_this_bot() {
        let parsed = BotCommand::parse("/status@telellm_bot", "telellm_bot")
            .expect("parse should succeed");

        assert_eq!(parsed, Some(BotCommand::Status));
    }

    #[test]
    fn parse_should_ignore_command_addressed_to_another_bot() {
        let parsed = BotCommand::parse("/status@other_bot", "telellm_bot")
            .expect("parse should succeed");

        assert_eq!(parsed, None);
    }

    #[test]
    fn parse_should_capture_forget_query() {
        let parsed =
            BotCommand::parse("/forget Mike hates cilantro", "telellm_bot").expect("parse should succeed");

        assert_eq!(
            parsed,
            Some(BotCommand::Forget {
                target: ForgetTarget::Query("Mike hates cilantro".to_owned())
            })
        );
    }
}
```

- [ ] **Step 2: Replace message addressing**

Use this content for `src/bot/message.rs`:

```rust
use crate::ids::{ChatId, MessageId, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    pub chat_id: ChatId,
    pub message_id: MessageId,
    pub from: Option<UserId>,
    pub from_name: Option<String>,
    pub text: String,
    pub reply_to_bot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressing {
    Ambient,
    Addressed,
}

impl IncomingMessage {
    pub fn addressing(&self, bot_username: &str) -> Addressing {
        if self.reply_to_bot || mentions_bot(&self.text, bot_username) {
            Addressing::Addressed
        } else {
            Addressing::Ambient
        }
    }
}

fn mentions_bot(text: &str, bot_username: &str) -> bool {
    let mention = format!("@{}", bot_username.trim_start_matches('@'));
    text.split_whitespace()
        .any(|word| word.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '@') == mention)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id: ChatId(-100),
            message_id: MessageId(1),
            from: Some(UserId(9)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            reply_to_bot: false,
        }
    }

    #[test]
    fn addressing_should_detect_bot_mention() {
        let msg = message("hey @telellm_bot what do you think?");

        assert_eq!(msg.addressing("telellm_bot"), Addressing::Addressed);
    }

    #[test]
    fn addressing_should_treat_plain_group_chat_as_ambient() {
        let msg = message("this is just group chat");

        assert_eq!(msg.addressing("telellm_bot"), Addressing::Ambient);
    }

    #[test]
    fn addressing_should_treat_reply_to_bot_as_addressed() {
        let mut msg = message("continue");
        msg.reply_to_bot = true;

        assert_eq!(msg.addressing("telellm_bot"), Addressing::Addressed);
    }
}
```

- [ ] **Step 3: Replace chunking with UTF-8 safe chunking**

Use this content for `src/bot/chunk.rs`:

```rust
pub fn chunk_for_telegram(input: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0, "telegram chunk limit must be greater than zero");
    if input.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for ch in input.chars() {
        if current.len() + ch.len_utf8() > limit && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_for_telegram_should_return_empty_for_empty_input() {
        let chunks = chunk_for_telegram("", 10);

        assert!(chunks.is_empty());
    }

    #[test]
    fn chunk_for_telegram_should_split_by_byte_limit_without_breaking_utf8() {
        let chunks = chunk_for_telegram("ab😀cd", 4);

        assert_eq!(chunks, vec!["ab".to_owned(), "😀".to_owned(), "cd".to_owned()]);
    }
}
```

- [ ] **Step 4: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 5: Commit**

Run:

```bash
git add src/bot
git commit -m "feat: add bot command and addressing logic"
```

---

### Task 4: Implement Rolling Memory And SQLite Memory Store

**Files:**
- Modify: `src/memory/rolling.rs`
- Modify: `src/memory/store.rs`
- Modify: `src/memory/sqlite.rs`
- Modify: `src/memory/mod.rs`

- [ ] **Step 1: Implement rolling buffer**

Replace `src/memory/rolling.rs` with:

```rust
use crate::{bot::message::IncomingMessage, ids::ChatId};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone)]
pub struct RollingBuffer {
    capacity: usize,
    messages: HashMap<ChatId, VecDeque<IncomingMessage>>,
}

impl RollingBuffer {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "rolling buffer capacity must be greater than zero");
        Self {
            capacity,
            messages: HashMap::new(),
        }
    }

    pub fn push(&mut self, message: IncomingMessage) {
        let queue = self.messages.entry(message.chat_id).or_default();
        queue.push_back(message);
        while queue.len() > self.capacity {
            queue.pop_front();
        }
    }

    pub fn recent_for_chat(&self, chat_id: ChatId) -> Vec<IncomingMessage> {
        self.messages
            .get(&chat_id)
            .map(|items| items.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MessageId, UserId};

    fn message(chat_id: ChatId, id: i32, text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id,
            message_id: MessageId(id),
            from: Some(UserId(1)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            reply_to_bot: false,
        }
    }

    #[test]
    fn recent_for_chat_should_keep_only_the_newest_messages() {
        let mut buffer = RollingBuffer::new(2);

        buffer.push(message(ChatId(1), 1, "one"));
        buffer.push(message(ChatId(1), 2, "two"));
        buffer.push(message(ChatId(1), 3, "three"));

        let recent = buffer.recent_for_chat(ChatId(1));
        assert_eq!(recent[0].text, "two");
    }
}
```

- [ ] **Step 2: Define durable memory store trait and types**

Replace `src/memory/store.rs` with:

```rust
use crate::ids::{ChatId, UserId};
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    pub id: i64,
    pub chat_id: ChatId,
    pub user_id: Option<UserId>,
    pub kind: MemoryKind,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryKind {
    Person,
    Preference,
    Relationship,
    GroupNorm,
    RunningJoke,
    Personality,
}

impl MemoryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Preference => "preference",
            Self::Relationship => "relationship",
            Self::GroupNorm => "group_norm",
            Self::RunningJoke => "running_joke",
            Self::Personality => "personality",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "person" => Some(Self::Person),
            "preference" => Some(Self::Preference),
            "relationship" => Some(Self::Relationship),
            "group_norm" => Some(Self::GroupNorm),
            "running_joke" => Some(Self::RunningJoke),
            "personality" => Some(Self::Personality),
            _ => None,
        }
    }
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn add_memory(
        &self,
        chat_id: ChatId,
        user_id: Option<UserId>,
        kind: MemoryKind,
        content: &str,
    ) -> Result<MemoryRecord, MemoryStoreError>;

    async fn list_memories(&self, chat_id: ChatId) -> Result<Vec<MemoryRecord>, MemoryStoreError>;

    async fn forget_all(&self, chat_id: ChatId) -> Result<u64, MemoryStoreError>;
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("invalid memory kind `{0}`")]
    InvalidKind(String),
}
```

- [ ] **Step 3: Implement SQLite store**

Replace `src/memory/sqlite.rs` with:

```rust
use super::store::{MemoryKind, MemoryRecord, MemoryStore, MemoryStoreError};
use crate::ids::{ChatId, UserId};
use async_trait::async_trait;
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};

#[derive(Debug, Clone)]
pub struct SqliteMemoryStore {
    pool: SqlitePool,
}

impl SqliteMemoryStore {
    pub async fn connect(database_url: &str) -> Result<Self, MemoryStoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<(), MemoryStoreError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chat_id INTEGER NOT NULL,
                user_id INTEGER,
                kind TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE INDEX IF NOT EXISTS idx_memories_chat_id ON memories(chat_id);
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    async fn add_memory(
        &self,
        chat_id: ChatId,
        user_id: Option<UserId>,
        kind: MemoryKind,
        content: &str,
    ) -> Result<MemoryRecord, MemoryStoreError> {
        let user_id_i64 = user_id.map(|id| id.0 as i64);
        let result = sqlx::query(
            "INSERT INTO memories (chat_id, user_id, kind, content) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(chat_id.0)
        .bind(user_id_i64)
        .bind(kind.as_str())
        .bind(content)
        .execute(&self.pool)
        .await?;

        Ok(MemoryRecord {
            id: result.last_insert_rowid(),
            chat_id,
            user_id,
            kind,
            content: content.to_owned(),
        })
    }

    async fn list_memories(&self, chat_id: ChatId) -> Result<Vec<MemoryRecord>, MemoryStoreError> {
        let rows = sqlx::query(
            "SELECT id, chat_id, user_id, kind, content FROM memories WHERE chat_id = ?1 ORDER BY id",
        )
        .bind(chat_id.0)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let kind_raw: String = row.get("kind");
                let kind = MemoryKind::from_str(&kind_raw)
                    .ok_or_else(|| MemoryStoreError::InvalidKind(kind_raw.clone()))?;
                let user_id_raw: Option<i64> = row.get("user_id");
                Ok(MemoryRecord {
                    id: row.get("id"),
                    chat_id: ChatId(row.get("chat_id")),
                    user_id: user_id_raw.map(|id| UserId(id as u64)),
                    kind,
                    content: row.get("content"),
                })
            })
            .collect()
    }

    async fn forget_all(&self, chat_id: ChatId) -> Result<u64, MemoryStoreError> {
        let result = sqlx::query("DELETE FROM memories WHERE chat_id = ?1")
            .bind(chat_id.0)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_memories_should_return_records_for_one_chat() {
        let store = SqliteMemoryStore::connect("sqlite::memory:")
            .await
            .expect("store should connect");

        store
            .add_memory(ChatId(1), Some(UserId(2)), MemoryKind::Preference, "likes terse answers")
            .await
            .expect("memory should insert");
        store
            .add_memory(ChatId(9), None, MemoryKind::GroupNorm, "unrelated")
            .await
            .expect("memory should insert");

        let memories = store.list_memories(ChatId(1)).await.expect("list should work");
        assert_eq!(memories[0].content, "likes terse answers");
    }

    #[tokio::test]
    async fn forget_all_should_remove_only_one_chat() {
        let store = SqliteMemoryStore::connect("sqlite::memory:")
            .await
            .expect("store should connect");

        store
            .add_memory(ChatId(1), None, MemoryKind::Personality, "dry humor")
            .await
            .expect("memory should insert");

        let removed = store.forget_all(ChatId(1)).await.expect("delete should work");
        assert_eq!(removed, 1);
    }
}
```

- [ ] **Step 4: Export memory types**

Replace `src/memory/mod.rs` with:

```rust
pub mod context;
pub mod rolling;
pub mod sqlite;
pub mod store;

pub use rolling::RollingBuffer;
pub use sqlite::SqliteMemoryStore;
pub use store::{MemoryKind, MemoryRecord, MemoryStore, MemoryStoreError};
```

- [ ] **Step 5: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 6: Commit**

Run:

```bash
git add src/memory
git commit -m "feat: add memory storage"
```

---

### Task 5: Build Prompt Context Packets

**Files:**
- Modify: `src/memory/context.rs`
- Modify: `src/codex/prompt.rs`

- [ ] **Step 1: Implement context packet builder**

Replace `src/memory/context.rs` with:

```rust
use super::store::MemoryRecord;
use crate::bot::message::IncomingMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPacket {
    pub triggering_message: IncomingMessage,
    pub recent_messages: Vec<IncomingMessage>,
    pub memories: Vec<MemoryRecord>,
}

impl ContextPacket {
    pub fn render(&self) -> String {
        let mut output = String::new();
        output.push_str("You are the Telegram group assistant for this chat.\n");
        output.push_str("Respond only to the triggering message. Use recent chat and memory as context.\n\n");

        output.push_str("Long-term memory:\n");
        if self.memories.is_empty() {
            output.push_str("- No durable memory is stored for this group yet.\n");
        } else {
            for memory in &self.memories {
                output.push_str("- ");
                output.push_str(memory.kind.as_str());
                output.push_str(": ");
                output.push_str(&memory.content);
                output.push('\n');
            }
        }

        output.push_str("\nRecent chat:\n");
        for message in &self.recent_messages {
            let name = message.from_name.as_deref().unwrap_or("unknown");
            output.push_str("- ");
            output.push_str(name);
            output.push_str(": ");
            output.push_str(&message.text);
            output.push('\n');
        }

        output.push_str("\nTriggering message:\n");
        let name = self.triggering_message.from_name.as_deref().unwrap_or("unknown");
        output.push_str(name);
        output.push_str(": ");
        output.push_str(&self.triggering_message.text);
        output.push('\n');
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ids::{ChatId, MessageId, UserId},
        memory::store::MemoryKind,
    };

    fn message(text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id: ChatId(1),
            message_id: MessageId(1),
            from: Some(UserId(2)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            reply_to_bot: false,
        }
    }

    #[test]
    fn render_should_include_memory_recent_chat_and_trigger() {
        let packet = ContextPacket {
            triggering_message: message("@telellm_bot summarize that"),
            recent_messages: vec![message("we were talking about the deploy")],
            memories: vec![MemoryRecord {
                id: 1,
                chat_id: ChatId(1),
                user_id: Some(UserId(2)),
                kind: MemoryKind::Preference,
                content: "Mike likes concise updates".to_owned(),
            }],
        };

        let rendered = packet.render();
        assert!(rendered.contains("Mike likes concise updates"));
    }
}
```

- [ ] **Step 2: Implement prompt envelope**

Replace `src/codex/prompt.rs` with:

```rust
use crate::memory::context::ContextPacket;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptEnvelope {
    body: String,
}

impl PromptEnvelope {
    pub fn from_context(packet: &ContextPacket) -> Self {
        Self {
            body: packet.render(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bot::message::IncomingMessage,
        ids::{ChatId, MessageId, UserId},
        memory::context::ContextPacket,
    };

    #[test]
    fn from_context_should_render_triggering_message() {
        let message = IncomingMessage {
            chat_id: ChatId(1),
            message_id: MessageId(1),
            from: Some(UserId(2)),
            from_name: Some("Mike".to_owned()),
            text: "@telellm_bot hello".to_owned(),
            reply_to_bot: false,
        };
        let packet = ContextPacket {
            triggering_message: message,
            recent_messages: Vec::new(),
            memories: Vec::new(),
        };

        let envelope = PromptEnvelope::from_context(&packet);
        assert!(envelope.as_str().contains("@telellm_bot hello"));
    }
}
```

- [ ] **Step 3: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 4: Commit**

Run:

```bash
git add src/memory/context.rs src/codex/prompt.rs
git commit -m "feat: build codex prompt context"
```

---

### Task 6: Implement Sandbox Backend Command Construction

**Files:**
- Modify: `src/sandbox/mod.rs`
- Modify: `src/sandbox/network.rs`
- Modify: `src/sandbox/docker.rs`

- [ ] **Step 1: Define sandbox trait**

Replace `src/sandbox/mod.rs` with:

```rust
pub mod docker;
pub mod network;

use crate::ids::{ChatId, SandboxId};
use async_trait::async_trait;

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
    pub broker_token: String,
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
            broker_token: "telellm-sandbox-token".to_owned(),
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
```

- [ ] **Step 2: Implement network policy script**

Replace `src/sandbox/network.rs` with:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkPolicy {
    pub blocked_cidrs: Vec<&'static str>,
    pub broker_host: String,
    pub broker_port: u16,
}

impl NetworkPolicy {
    pub fn new(broker_host: impl Into<String>, broker_port: u16) -> Self {
        Self {
            blocked_cidrs: vec![
                "0.0.0.0/8",
                "10.0.0.0/8",
                "127.0.0.0/8",
                "169.254.0.0/16",
                "172.16.0.0/12",
                "192.168.0.0/16",
                "224.0.0.0/4",
            ],
            broker_host: broker_host.into(),
            broker_port,
        }
    }

    pub fn enforcement_script(&self) -> String {
        let mut script = String::from("#!/usr/bin/env sh\nset -eu\n");
        script.push_str("iptables -P OUTPUT ACCEPT\n");
        script.push_str("if [ -n \"${TELELLM_BROKER_HOST:-}\" ]; then\n");
        script.push_str("  broker_ip=$(getent hosts \"$TELELLM_BROKER_HOST\" | awk '{ print $1 }' | head -n 1)\n");
        script.push_str("  if [ -n \"$broker_ip\" ]; then\n");
        script.push_str("    iptables -A OUTPUT -d \"$broker_ip\" -p tcp --dport \"${TELELLM_BROKER_PORT:-8189}\" -j ACCEPT\n");
        script.push_str("  fi\n");
        script.push_str("fi\n");
        for cidr in &self.blocked_cidrs {
            script.push_str("iptables -A OUTPUT -d ");
            script.push_str(cidr);
            script.push_str(" -j REJECT\n");
        }
        script.push_str("exec \"$@\"\n");
        script
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforcement_script_should_reject_private_lan_ranges() {
        let policy = NetworkPolicy::new("host.docker.internal", 8189);

        let script = policy.enforcement_script();
        assert!(script.contains("iptables -A OUTPUT -d 192.168.0.0/16 -j REJECT"));
    }

    #[test]
    fn enforcement_script_should_allow_configured_broker_before_reject_rules() {
        let policy = NetworkPolicy::new("host.docker.internal", 8189);

        let script = policy.enforcement_script();
        assert!(script.contains("TELELLM_BROKER_HOST"));
    }
}
```

- [ ] **Step 3: Implement Docker command builder and backend shell**

Replace `src/sandbox/docker.rs` with:

```rust
use super::{SandboxBackend, SandboxError, SandboxSpec};
use async_trait::async_trait;
use tokio::process::Command;

#[derive(Debug, Clone, Default)]
pub struct DockerSandboxBackend;

impl DockerSandboxBackend {
    pub fn run_args(spec: &SandboxSpec) -> Vec<String> {
        vec![
            "run".to_owned(),
            "-d".to_owned(),
            "--name".to_owned(),
            spec.sandbox_id.to_string(),
            "--network".to_owned(),
            spec.network.clone(),
            "--cap-add".to_owned(),
            "NET_ADMIN".to_owned(),
            "--security-opt".to_owned(),
            "no-new-privileges".to_owned(),
            "-v".to_owned(),
            format!("{}:/workspace", spec.workspace_volume),
            "-e".to_owned(),
            format!("TELELLM_BROKER_HOST={}", spec.broker_host),
            "-e".to_owned(),
            format!("TELELLM_BROKER_PORT={}", spec.broker_port),
            "-e".to_owned(),
            format!("OPENAI_BASE_URL={}", spec.broker_base_url),
            "-e".to_owned(),
            format!("OPENAI_API_KEY={}", spec.broker_token),
            "-w".to_owned(),
            "/workspace".to_owned(),
            spec.image.clone(),
            "sleep".to_owned(),
            "infinity".to_owned(),
        ]
    }

    pub fn exec_args(spec: &SandboxSpec, command: &[&str]) -> Vec<String> {
        let mut args = vec!["exec".to_owned(), "-i".to_owned(), spec.sandbox_id.to_string()];
        args.extend(command.iter().map(|part| (*part).to_owned()));
        args
    }

    async fn docker(args: &[String]) -> Result<(), SandboxError> {
        let output = Command::new("docker").args(args).output().await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(SandboxError::Docker(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }
}

#[async_trait]
impl SandboxBackend for DockerSandboxBackend {
    async fn ensure_started(&self, spec: &SandboxSpec) -> Result<(), SandboxError> {
        Self::docker(&Self::run_args(spec)).await
    }

    async fn restart(&self, spec: &SandboxSpec) -> Result<(), SandboxError> {
        Self::docker(&["restart".to_owned(), spec.sandbox_id.to_string()]).await
    }

    async fn rebuild(&self, spec: &SandboxSpec, clear_workspace: bool) -> Result<(), SandboxError> {
        let _ = Self::docker(&["rm".to_owned(), "-f".to_owned(), spec.sandbox_id.to_string()]).await;
        if clear_workspace {
            let _ = Self::docker(&["volume".to_owned(), "rm".to_owned(), spec.workspace_volume.clone()]).await;
        }
        self.ensure_started(spec).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ChatId;

    #[test]
    fn run_args_should_not_mount_host_paths_or_docker_socket() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::run_args(&spec).join(" ");
        assert!(!args.contains("/var/run/docker.sock"));
    }

    #[test]
    fn run_args_should_mount_only_named_workspace_volume() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::run_args(&spec);
        assert!(args.contains(&"vol_telellm-chat-1:/workspace".to_owned()));
    }

    #[test]
    fn run_args_should_expose_only_the_broker_exception_to_entrypoint() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::run_args(&spec);
        assert!(args.contains(&"TELELLM_BROKER_HOST=host.docker.internal".to_owned()));
    }

    #[test]
    fn run_args_should_point_codex_at_host_broker() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::run_args(&spec);
        assert!(args.contains(&"OPENAI_BASE_URL=http://host.docker.internal:8189/v1".to_owned()));
    }
}
```

- [ ] **Step 4: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 5: Commit**

Run:

```bash
git add src/sandbox
git commit -m "feat: add docker sandbox backend"
```

---

### Task 7: Implement PTY-Backed Codex Session Supervision

**Files:**
- Modify: `src/codex/session.rs`
- Modify: `src/codex/pty.rs`
- Create: `tests/pty_session.rs`

- [ ] **Step 1: Define Codex session trait**

Replace `src/codex/session.rs` with:

```rust
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
    #[error("session closed")]
    Closed,
}
```

- [ ] **Step 2: Implement a PTY session with serialized sends**

Replace `src/codex/pty.rs` with:

```rust
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
            guard.writer.write_all(request.prompt.as_bytes()).map_err(|err| {
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
```

- [ ] **Step 3: Add PTY integration test**

Create `tests/pty_session.rs`:

```rust
use telellm::codex::{
    pty::PtyCodexSession,
    session::{CodexRequest, CodexSession},
};

#[tokio::test]
async fn pty_session_should_round_trip_input_to_cat_process() {
    let session = PtyCodexSession::spawn("cat", &[]).expect("cat session should spawn");

    let turn = session
        .send(CodexRequest {
            prompt: "hello from pty".to_owned(),
        })
        .await
        .expect("send should succeed");

    assert!(turn.output.contains("hello from pty"));
}
```

- [ ] **Step 4: Run the PTY test**

Run:

```bash
cargo test --test pty_session
```

Expected: PASS.

- [ ] **Step 5: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 6: Commit**

Run:

```bash
git add src/codex tests/pty_session.rs
git commit -m "feat: add pty codex session"
```

---

### Task 8: Implement Per-Group Router Serialization

**Files:**
- Modify: `src/router.rs`
- Modify: `src/bot/telegram.rs`

- [ ] **Step 1: Define Telegram sink test double boundary**

Replace `src/bot/telegram.rs` with:

```rust
use crate::ids::ChatId;
use async_trait::async_trait;

#[async_trait]
pub trait TelegramSink: Send + Sync {
    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), TelegramError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("telegram send failed: {0}")]
    Send(String),
}

pub struct TelegramAdapter;
```

- [ ] **Step 2: Implement router with tests**

Replace `src/router.rs` with:

```rust
use crate::{
    bot::telegram::{TelegramError, TelegramSink},
    codex::session::{CodexRequest, CodexSession, CodexSessionError},
    ids::ChatId,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, Clone)]
pub struct GroupWorkItem {
    pub chat_id: ChatId,
    pub prompt: String,
}

pub struct Router<S, T>
where
    S: CodexSession + 'static,
    T: TelegramSink + 'static,
{
    queue_depth: usize,
    sessions: Arc<Mutex<HashMap<ChatId, Arc<S>>>>,
    senders: Arc<Mutex<HashMap<ChatId, mpsc::Sender<GroupWorkItem>>>>,
    telegram: Arc<T>,
}

impl<S, T> Router<S, T>
where
    S: CodexSession + 'static,
    T: TelegramSink + 'static,
{
    pub fn new(queue_depth: usize, telegram: Arc<T>) -> Self {
        Self {
            queue_depth,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            telegram,
        }
    }

    pub async fn register_session(&self, chat_id: ChatId, session: Arc<S>) {
        self.sessions.lock().await.insert(chat_id, session);
    }

    pub async fn enqueue(&self, item: GroupWorkItem) -> Result<(), RouterError> {
        let sender = self.sender_for_chat(item.chat_id).await?;
        sender.send(item).await.map_err(|_| RouterError::QueueClosed)
    }

    async fn sender_for_chat(&self, chat_id: ChatId) -> Result<mpsc::Sender<GroupWorkItem>, RouterError> {
        if let Some(sender) = self.senders.lock().await.get(&chat_id).cloned() {
            return Ok(sender);
        }

        let session = self
            .sessions
            .lock()
            .await
            .get(&chat_id)
            .cloned()
            .ok_or(RouterError::MissingSession(chat_id))?;
        let telegram = self.telegram.clone();
        let (tx, mut rx) = mpsc::channel(self.queue_depth);

        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                match session
                    .send(CodexRequest {
                        prompt: item.prompt,
                    })
                    .await
                {
                    Ok(turn) => {
                        let _ = telegram.send_message(item.chat_id, &turn.output).await;
                    }
                    Err(err) => {
                        let _ = telegram
                            .send_message(item.chat_id, &format!("Codex session failed: {err}"))
                            .await;
                    }
                }
            }
        });

        self.senders.lock().await.insert(chat_id, tx.clone());
        Ok(tx)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("no Codex session registered for chat {0:?}")]
    MissingSession(ChatId),
    #[error("group queue is closed")]
    QueueClosed,
    #[error("Codex error: {0}")]
    Codex(#[from] CodexSessionError),
    #[error("Telegram error: {0}")]
    Telegram(#[from] TelegramError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::session::CodexTurn;
    use async_trait::async_trait;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct FakeSession {
        prompts: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl CodexSession for FakeSession {
        async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
            self.prompts.lock().await.push(request.prompt.clone());
            Ok(CodexTurn {
                output: format!("reply: {}", request.prompt),
            })
        }

        async fn restart(&self) -> Result<(), CodexSessionError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeTelegram {
        messages: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl TelegramSink for FakeTelegram {
        async fn send_message(&self, _chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
            self.messages.lock().await.push(text.to_owned());
            Ok(())
        }
    }

    #[tokio::test]
    async fn enqueue_should_send_work_to_registered_session() {
        let telegram = Arc::new(FakeTelegram::default());
        let session = Arc::new(FakeSession::default());
        let router = Router::new(4, telegram.clone());
        router.register_session(ChatId(1), session).await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "hello".to_owned(),
            })
            .await
            .expect("enqueue should work");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(telegram.messages.lock().await[0], "reply: hello");
    }
}
```

- [ ] **Step 3: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 4: Commit**

Run:

```bash
git add src/router.rs src/bot/telegram.rs
git commit -m "feat: add per-group router"
```

---

### Task 9: Implement Host Auth Broker Skeleton

**Files:**
- Modify: `src/broker.rs`

- [ ] **Step 1: Add broker route and forwarding model**

Replace `src/broker.rs` with:

```rust
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use std::{net::SocketAddr, sync::Arc};

#[derive(Debug, Clone)]
pub struct BrokerConfig {
    pub listen: SocketAddr,
    pub upstream_base_url: String,
    pub upstream_api_key: SecretString,
}

#[derive(Clone)]
pub struct BrokerState {
    client: Client,
    upstream_base_url: String,
    upstream_api_key: SecretString,
}

impl BrokerState {
    pub fn new(config: BrokerConfig) -> Self {
        Self {
            client: Client::new(),
            upstream_base_url: config.upstream_base_url,
            upstream_api_key: config.upstream_api_key,
        }
    }
}

pub fn router(state: BrokerState) -> Router {
    Router::new()
        .route("/v1/responses", post(proxy_responses))
        .route("/v1/chat/completions", post(proxy_chat_completions))
        .with_state(Arc::new(state))
}

async fn proxy_responses(State(state): State<Arc<BrokerState>>, headers: HeaderMap, body: Bytes) -> Response {
    proxy_to_upstream(state, headers, body, "/responses").await
}

async fn proxy_chat_completions(
    State(state): State<Arc<BrokerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy_to_upstream(state, headers, body, "/chat/completions").await
}

async fn proxy_to_upstream(
    state: Arc<BrokerState>,
    _headers: HeaderMap,
    body: Bytes,
    path: &str,
) -> Response {
    let url = format!("{}{}", state.upstream_base_url.trim_end_matches('/'), path);
    let result = state
        .client
        .post(url)
        .bearer_auth(state.upstream_api_key.expose_secret())
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await;

    match result {
        Ok(response) => {
            let status = response.status();
            match response.bytes().await {
                Ok(bytes) => (status, bytes).into_response(),
                Err(err) => (
                    StatusCode::BAD_GATEWAY,
                    format!("failed reading upstream response: {err}"),
                )
                    .into_response(),
            }
        }
        Err(err) => (StatusCode::BAD_GATEWAY, format!("upstream request failed: {err}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn router_should_return_bad_gateway_when_upstream_is_unavailable() {
        let app = router(BrokerState::new(BrokerConfig {
            listen: "127.0.0.1:0".parse().expect("listen addr should parse"),
            upstream_base_url: "http://127.0.0.1:9/v1".to_owned(),
            upstream_api_key: SecretString::from("test-key"),
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("response should be returned");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
}
```

- [ ] **Step 2: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 3: Commit**

Run:

```bash
git add src/broker.rs
git commit -m "feat: add host auth broker"
```

---

### Task 10: Wire App-Level Message Flow With Test Doubles

**Files:**
- Modify: `src/app.rs`
- Modify: `src/bot/mod.rs`

- [ ] **Step 1: Add app orchestration types**

Replace `src/app.rs` with:

```rust
use crate::{
    bot::{
        command::BotCommand,
        message::{Addressing, IncomingMessage},
    },
    config::AppConfig,
    memory::{context::ContextPacket, MemoryStore, RollingBuffer},
    router::{GroupWorkItem, Router, RouterError},
};
use std::sync::Arc;
use tokio::sync::Mutex;

pub async fn run(_config: AppConfig) -> anyhow::Result<()> {
    tracing::info!("telellm daemon configuration loaded");
    Ok(())
}

pub struct AppCore<M, R> {
    bot_username: String,
    memory_store: Arc<M>,
    rolling: Arc<Mutex<RollingBuffer>>,
    router: Arc<R>,
}

impl<M, R> AppCore<M, R> {
    pub fn new(bot_username: String, memory_store: Arc<M>, rolling: RollingBuffer, router: Arc<R>) -> Self {
        Self {
            bot_username,
            memory_store,
            rolling: Arc::new(Mutex::new(rolling)),
            router,
        }
    }
}

impl<M, S, T> AppCore<M, Router<S, T>>
where
    M: MemoryStore + 'static,
    S: crate::codex::session::CodexSession + 'static,
    T: crate::bot::telegram::TelegramSink + 'static,
{
    pub async fn handle_message(&self, message: IncomingMessage) -> Result<(), AppError> {
        self.rolling.lock().await.push(message.clone());

        if let Some(command) = BotCommand::parse(&message.text, &self.bot_username)? {
            return self.handle_command(message, command).await;
        }

        if message.addressing(&self.bot_username) == Addressing::Ambient {
            return Ok(());
        }

        let recent_messages = self.rolling.lock().await.recent_for_chat(message.chat_id);
        let memories = self.memory_store.list_memories(message.chat_id).await?;
        let packet = ContextPacket {
            triggering_message: message.clone(),
            recent_messages,
            memories,
        };

        self.router
            .enqueue(GroupWorkItem {
                chat_id: message.chat_id,
                prompt: packet.render(),
            })
            .await?;

        Ok(())
    }

    async fn handle_command(&self, message: IncomingMessage, command: BotCommand) -> Result<(), AppError> {
        match command {
            BotCommand::Help => {
                self.router
                    .enqueue(GroupWorkItem {
                        chat_id: message.chat_id,
                        prompt: "Show concise help for /status /reset /restart /rebuild /memory /forget.".to_owned(),
                    })
                    .await?;
            }
            BotCommand::Memory => {
                let memories = self.memory_store.list_memories(message.chat_id).await?;
                let prompt = format!("Summarize these durable group memories: {memories:?}");
                self.router
                    .enqueue(GroupWorkItem {
                        chat_id: message.chat_id,
                        prompt,
                    })
                    .await?;
            }
            BotCommand::Forget { .. } => {
                self.memory_store.forget_all(message.chat_id).await?;
            }
            BotCommand::Status | BotCommand::Reset { .. } | BotCommand::Restart | BotCommand::Rebuild { .. } => {
                self.router
                    .enqueue(GroupWorkItem {
                        chat_id: message.chat_id,
                        prompt: format!("Acknowledge command: {command:?}"),
                    })
                    .await?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("command parse error: {0}")]
    Command(#[from] crate::bot::command::CommandParseError),
    #[error("memory error: {0}")]
    Memory(#[from] crate::memory::MemoryStoreError),
    #[error("router error: {0}")]
    Router(#[from] RouterError),
}
```

- [ ] **Step 2: Ensure bot exports are clean**

Replace `src/bot/mod.rs` with:

```rust
pub mod chunk;
pub mod command;
pub mod message;
pub mod telegram;
```

- [ ] **Step 3: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 4: Commit**

Run:

```bash
git add src/app.rs src/bot/mod.rs
git commit -m "feat: wire app message flow"
```

---

### Task 11: Add Telegram Adapter And Reply Chunking

**Files:**
- Modify: `src/bot/telegram.rs`
- Modify: `src/bot/chunk.rs`

- [ ] **Step 1: Add teloxide-backed sink**

Extend `src/bot/telegram.rs` with this implementation below the trait definitions:

```rust
use crate::bot::chunk::chunk_for_telegram;
use teloxide::{prelude::Requester, types::ChatId as TeloxideChatId, Bot};

pub struct TeloxideTelegramSink {
    bot: Bot,
    chunk_limit: usize,
}

impl TeloxideTelegramSink {
    pub fn new(bot: Bot, chunk_limit: usize) -> Self {
        Self { bot, chunk_limit }
    }
}

#[async_trait]
impl TelegramSink for TeloxideTelegramSink {
    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
        for chunk in chunk_for_telegram(text, self.chunk_limit) {
            self.bot
                .send_message(TeloxideChatId(chat_id.0), chunk)
                .await
                .map_err(|err| TelegramError::Send(err.to_string()))?;
        }
        Ok(())
    }
}
```

- [ ] **Step 2: Add chunking test for Telegram limit**

Add this test to `src/bot/chunk.rs`:

```rust
#[cfg(test)]
mod telegram_limit_tests {
    use super::*;

    #[test]
    fn chunk_for_telegram_should_keep_each_chunk_under_limit() {
        let chunks = chunk_for_telegram(&"x".repeat(9000), 3900);

        assert!(chunks.iter().all(|chunk| chunk.len() <= 3900));
    }
}
```

- [ ] **Step 3: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 4: Commit**

Run:

```bash
git add src/bot
git commit -m "feat: add telegram sink"
```

---

### Task 12: Add Sandbox Image And Docker Integration Tests

**Files:**
- Create: `Dockerfile.sandbox`
- Create: `.dockerignore`
- Create: `tests/docker_sandbox.rs`

- [ ] **Step 1: Create Dockerfile**

Create `Dockerfile.sandbox`:

```Dockerfile
FROM node:24-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        iproute2 \
        iptables \
        procps \
        python3 \
        openssh-client \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g @openai/codex

RUN useradd --create-home --shell /bin/bash codex
WORKDIR /workspace

COPY scripts/sandbox-entrypoint.sh /usr/local/bin/sandbox-entrypoint.sh
RUN chmod +x /usr/local/bin/sandbox-entrypoint.sh

USER root
ENTRYPOINT ["/usr/local/bin/sandbox-entrypoint.sh"]
CMD ["sleep", "infinity"]
```

- [ ] **Step 2: Create Docker ignore file**

Create `.dockerignore`:

```dockerignore
.git
target
data
docs/superpowers/plans
```

- [ ] **Step 3: Create sandbox entrypoint script**

Create `scripts/sandbox-entrypoint.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TELELLM_BROKER_HOST:-}" ]]; then
  broker_ip="$(getent hosts "$TELELLM_BROKER_HOST" | awk '{ print $1 }' | head -n 1)"
  if [[ -n "$broker_ip" ]]; then
    iptables -A OUTPUT -d "$broker_ip" -p tcp --dport "${TELELLM_BROKER_PORT:-8189}" -j ACCEPT
  fi
fi

for cidr in \
  0.0.0.0/8 \
  10.0.0.0/8 \
  127.0.0.0/8 \
  169.254.0.0/16 \
  172.16.0.0/12 \
  192.168.0.0/16 \
  224.0.0.0/4
do
  iptables -A OUTPUT -d "$cidr" -j REJECT
done

exec "$@"
```

- [ ] **Step 4: Add ignored Docker integration tests**

Create `tests/docker_sandbox.rs`:

```rust
use std::process::Command;

#[test]
#[ignore = "requires Docker/Colima and the telellm-sandbox:local image"]
fn sandbox_should_block_private_lan_address() {
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--cap-add",
            "NET_ADMIN",
            "telellm-sandbox:local",
            "sh",
            "-lc",
            "curl --max-time 2 http://192.168.1.1",
        ])
        .output()
        .expect("docker should run");

    assert!(!output.status.success());
}

#[test]
#[ignore = "requires Docker/Colima and the telellm-sandbox:local image"]
fn sandbox_should_allow_public_internet() {
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--cap-add",
            "NET_ADMIN",
            "telellm-sandbox:local",
            "sh",
            "-lc",
            "curl --max-time 10 -fsS https://api.openai.com",
        ])
        .output()
        .expect("docker should run");

    assert!(output.status.success());
}
```

- [ ] **Step 5: Build sandbox image**

Run:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
```

Expected: image builds and includes `codex`.

- [ ] **Step 6: Run ignored Docker tests manually**

Run:

```bash
cargo test --test docker_sandbox -- --ignored
```

Expected: public internet test passes; private LAN test fails the curl command and passes the assertion.

- [ ] **Step 7: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 8: Commit**

Run:

```bash
git add Dockerfile.sandbox .dockerignore scripts/sandbox-entrypoint.sh tests/docker_sandbox.rs
git commit -m "feat: add sandbox image"
```

---

### Task 13: Add Per-Chat Runtime Manager

**Files:**
- Create: `src/runtime.rs`
- Modify: `src/lib.rs`
- Modify: `src/app.rs`

- [ ] **Step 1: Export runtime module**

Add this line to `src/lib.rs`:

```rust
pub mod runtime;
```

- [ ] **Step 2: Implement runtime manager**

Replace `src/runtime.rs` with:

```rust
use crate::{
    bot::telegram::TelegramSink,
    codex::pty::PtyCodexSession,
    config::{CodexConfig, DockerConfig},
    ids::ChatId,
    router::Router,
    sandbox::{
        docker::DockerSandboxBackend, SandboxBackend, SandboxError, SandboxSpec,
    },
};
use std::{collections::HashSet, sync::Arc};
use tokio::sync::Mutex;

#[async_trait::async_trait]
pub trait RuntimeEnsurer: Send + Sync {
    async fn ensure_chat_runtime(&self, chat_id: ChatId) -> Result<(), RuntimeError>;
}

pub struct RuntimeManager<T>
where
    T: TelegramSink + 'static,
{
    docker: DockerSandboxBackend,
    docker_config: DockerConfig,
    codex_config: CodexConfig,
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
        router: Arc<Router<PtyCodexSession, T>>,
    ) -> Self {
        Self {
            docker,
            docker_config,
            codex_config,
            router,
            registered: Mutex::new(HashSet::new()),
        }
    }

    async fn ensure_chat_runtime_inner(&self, chat_id: ChatId) -> Result<(), RuntimeError> {
        if self.registered.lock().await.contains(&chat_id) {
            return Ok(());
        }

        let spec = SandboxSpec::new(
            chat_id,
            &self.docker_config.image,
            &self.docker_config.network,
            &self.docker_config.workspace_volume_prefix,
        );
        self.docker.ensure_started(&spec).await?;

        let mut codex_parts = vec![self.codex_config.command.as_str()];
        codex_parts.extend(self.codex_config.args.iter().map(String::as_str));
        codex_parts.extend(["--cd", "/workspace"]);

        let docker_args = DockerSandboxBackend::exec_args(&spec, &codex_parts);
        let session = Arc::new(PtyCodexSession::spawn("docker", &docker_args)?);
        self.router.register_session(chat_id, session).await;
        self.registered.lock().await.insert(chat_id);
        Ok(())
    }
}

#[async_trait::async_trait]
impl<T> RuntimeEnsurer for RuntimeManager<T>
where
    T: TelegramSink + 'static,
{
    async fn ensure_chat_runtime(&self, chat_id: ChatId) -> Result<(), RuntimeError> {
        self.ensure_chat_runtime_inner(chat_id).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("sandbox error: {0}")]
    Sandbox(#[from] SandboxError),
    #[error("Codex session error: {0}")]
    Codex(#[from] crate::codex::session::CodexSessionError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CodexConfig, DockerConfig};
    use std::collections::BTreeMap;

    #[test]
    fn codex_parts_should_be_representable_as_docker_exec_command() {
        let docker = DockerConfig {
            image: "telellm-sandbox:local".to_owned(),
            network: "telellm_public".to_owned(),
            workspace_volume_prefix: "telellm_workspace".to_owned(),
        };
        let codex = CodexConfig {
            command: "codex".to_owned(),
            args: vec!["--no-alt-screen".to_owned()],
            model: "gpt-5-codex".to_owned(),
            env: BTreeMap::new(),
        };
        let spec = SandboxSpec::new(
            ChatId(1),
            &docker.image,
            &docker.network,
            &docker.workspace_volume_prefix,
        );
        let mut parts = vec![codex.command.as_str()];
        parts.extend(codex.args.iter().map(String::as_str));
        parts.extend(["--cd", "/workspace"]);

        let args = DockerSandboxBackend::exec_args(&spec, &parts);
        assert!(args.contains(&"codex".to_owned()));
    }
}
```

- [ ] **Step 3: Wire `AppCore` to ensure runtime before routed work**

Modify the `AppCore` struct in `src/app.rs` to include a runtime ensurer:

```rust
use crate::runtime::RuntimeEnsurer;

pub struct AppCore<M, R, N> {
    bot_username: String,
    memory_store: Arc<M>,
    rolling: Arc<Mutex<RollingBuffer>>,
    router: Arc<R>,
    runtime: Arc<N>,
}

impl<M, R, N> AppCore<M, R, N> {
    pub fn new(
        bot_username: String,
        memory_store: Arc<M>,
        rolling: RollingBuffer,
        router: Arc<R>,
        runtime: Arc<N>,
    ) -> Self {
        Self {
            bot_username,
            memory_store,
            rolling: Arc::new(Mutex::new(rolling)),
            router,
            runtime,
        }
    }
}
```

Change the `handle_message` impl header and early flow to this shape:

```rust
impl<M, S, T, N> AppCore<M, Router<S, T>, N>
where
    M: MemoryStore + 'static,
    S: crate::codex::session::CodexSession + 'static,
    T: crate::bot::telegram::TelegramSink + 'static,
    N: RuntimeEnsurer + 'static,
{
    pub async fn handle_message(&self, message: IncomingMessage) -> Result<(), AppError> {
        self.rolling.lock().await.push(message.clone());
        let command = BotCommand::parse(&message.text, &self.bot_username)?;

        if command.is_none() && message.addressing(&self.bot_username) == Addressing::Ambient {
            return Ok(());
        }

        self.runtime.ensure_chat_runtime(message.chat_id).await?;

        if let Some(command) = command {
            return self.handle_command(message, command).await;
        }

        let recent_messages = self.rolling.lock().await.recent_for_chat(message.chat_id);
        let memories = self.memory_store.list_memories(message.chat_id).await?;
        let packet = ContextPacket {
            triggering_message: message.clone(),
            recent_messages,
            memories,
        };

        self.router
            .enqueue(GroupWorkItem {
                chat_id: message.chat_id,
                prompt: packet.render(),
            })
            .await?;

        Ok(())
    }
}
```

Add the runtime error variant to `AppError`:

```rust
#[error("runtime error: {0}")]
Runtime(#[from] crate::runtime::RuntimeError),
```

- [ ] **Step 4: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 5: Commit**

Run:

```bash
git add src/lib.rs src/runtime.rs src/app.rs
git commit -m "feat: add chat runtime manager"
```

---

### Task 14: Add Telegram Polling Adapter

**Files:**
- Modify: `src/bot/telegram.rs`

- [ ] **Step 1: Add incoming message handler and polling loop**

Extend `src/bot/telegram.rs` with:

```rust
use crate::{
    bot::message::IncomingMessage,
    ids::{MessageId, UserId},
};
use std::sync::Arc;
use teloxide::{
    dispatching::UpdateFilterExt,
    prelude::*,
    types::{Message, User},
};

#[async_trait]
pub trait IncomingMessageHandler: Send + Sync {
    async fn handle_message(&self, message: IncomingMessage);
}

pub async fn run_polling(
    bot: Bot,
    bot_username: String,
    handler: Arc<dyn IncomingMessageHandler>,
) -> Result<(), TelegramError> {
    let handler_filter = Update::filter_message().endpoint(move |msg: Message| {
        let handler = handler.clone();
        let bot_username = bot_username.clone();
        async move {
            if let Some(incoming) = normalize_message(&msg, &bot_username) {
                handler.handle_message(incoming).await;
            }
            respond(())
        }
    });

    Dispatcher::builder(bot, handler_filter)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}

fn normalize_message(message: &Message, bot_username: &str) -> Option<IncomingMessage> {
    let text = message.text()?.to_owned();
    let from = message.from.as_ref().map(user_id);
    let from_name = message.from.as_ref().map(display_name);
    let reply_to_bot = message
        .reply_to_message()
        .and_then(|reply| reply.from.as_ref())
        .and_then(|user| user.username.as_deref())
        .is_some_and(|username| username.eq_ignore_ascii_case(bot_username));

    Some(IncomingMessage {
        chat_id: crate::ids::ChatId(message.chat.id.0),
        message_id: MessageId(message.id.0),
        from,
        from_name,
        text,
        reply_to_bot,
    })
}

fn user_id(user: &User) -> UserId {
    UserId(user.id.0)
}

fn display_name(user: &User) -> String {
    match &user.last_name {
        Some(last_name) => format!("{} {}", user.first_name, last_name),
        None => user.first_name.clone(),
    }
}
```

- [ ] **Step 2: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 3: Commit**

Run:

```bash
git add src/bot/telegram.rs
git commit -m "feat: add telegram polling adapter"
```

---

### Task 15: Wire Real App Startup

**Files:**
- Modify: `src/app.rs`
- Modify: `src/main.rs`
- Create: `README.md`

- [ ] **Step 1: Add app handler wrapper**

Extend `src/app.rs` with:

```rust
use crate::bot::telegram::IncomingMessageHandler;

#[async_trait::async_trait]
impl<M, S, T, N> IncomingMessageHandler for AppCore<M, Router<S, T>, N>
where
    M: MemoryStore + 'static,
    S: crate::codex::session::CodexSession + 'static,
    T: crate::bot::telegram::TelegramSink + 'static,
    N: crate::runtime::RuntimeEnsurer + 'static,
{
    async fn handle_message(&self, message: IncomingMessage) {
        if let Err(err) = AppCore::handle_message(self, message).await {
            tracing::warn!(error = %err, "failed to handle Telegram message");
        }
    }
}
```

- [ ] **Step 2: Update `src/app.rs` startup**

Replace `run` in `src/app.rs` with:

```rust
pub async fn run(config: AppConfig) -> anyhow::Result<()> {
    let telegram_token = config.telegram_token_from_env()?;
    let upstream_api_key = std::env::var(&config.broker.upstream_api_key_env)?;
    let broker_state = crate::broker::BrokerState::new(crate::broker::BrokerConfig {
        listen: config.broker.listen,
        upstream_base_url: config.broker.upstream_base_url.clone(),
        upstream_api_key: secrecy::SecretString::from(upstream_api_key),
    });
    let broker_listener = tokio::net::TcpListener::bind(config.broker.listen).await?;
    tokio::spawn(async move {
        if let Err(err) = axum::serve(broker_listener, crate::broker::router(broker_state)).await {
            tracing::error!(error = %err, "broker server stopped");
        }
    });

    let bot = teloxide::Bot::new(secrecy::ExposeSecret::expose_secret(&telegram_token).to_owned());
    let telegram_sink = std::sync::Arc::new(crate::bot::telegram::TeloxideTelegramSink::new(
        bot.clone(),
        config.limits.telegram_chunk_chars,
    ));
    let router = std::sync::Arc::new(crate::router::Router::new(
        config.limits.per_group_queue_depth,
        telegram_sink,
    ));
    let memory_url = format!("sqlite://{}", config.storage.sqlite_path.display());
    let memory_store =
        std::sync::Arc::new(crate::memory::SqliteMemoryStore::connect(&memory_url).await?);
    let runtime = std::sync::Arc::new(crate::runtime::RuntimeManager::new(
        crate::sandbox::docker::DockerSandboxBackend,
        config.docker.clone(),
        config.codex.clone(),
        router.clone(),
    ));
    let app_core = std::sync::Arc::new(AppCore::new(
        config.telegram.bot_username.clone(),
        memory_store,
        crate::memory::RollingBuffer::new(config.limits.recent_buffer_messages),
        router,
        runtime,
    ));

    crate::bot::telegram::run_polling(bot, config.telegram.bot_username.clone(), app_core).await?;

    Ok(())
}
```

- [ ] **Step 3: Add README**

Create `README.md`:

```markdown
# telellm

`telellm` bridges Telegram group chats to sandboxed Codex CLI sessions.

## Local Setup

1. Copy `config.example.toml` to `config.toml`.
2. Set `TELEGRAM_BOT_TOKEN`.
3. Set `OPENAI_API_KEY` for the host broker.
4. Build the sandbox image:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
```

5. Run checks:

```bash
./scripts/check.sh
```

6. Start the daemon:

```bash
cargo run -- --config config.toml
```

## Security Model

Each Telegram group maps to a Docker/Colima sandbox and persistent workspace volume. The daemon owns Telegram credentials, durable memory, and long-lived provider credentials. The sandbox receives only scoped access to the host broker and must not receive arbitrary host mounts or the Docker socket.
```

- [ ] **Step 4: Run validation**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 5: Commit**

Run:

```bash
git add src/app.rs src/main.rs README.md
git commit -m "feat: wire daemon startup"
```

---

### Task 16: Final Spec Coverage Verification

**Files:**
- Modify: `docs/superpowers/plans/2026-04-30-telegram-codex-bridge-implementation.md`

- [ ] **Step 1: Run all checks**

Run:

```bash
./scripts/check.sh
```

Expected: PASS.

- [ ] **Step 2: Run Docker checks when Docker/Colima is available**

Run:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
cargo test --test docker_sandbox -- --ignored
```

Expected: image builds, public internet is reachable, private LAN probe is rejected.

- [ ] **Step 3: Confirm spec coverage**

Check these points manually against `docs/superpowers/specs/2026-04-30-telegram-codex-bridge-design.md`:

- Telegram bot command parsing and addressing rules are present.
- Per-group routing serializes prompts.
- Rolling memory and durable SQLite memory exist.
- Context packets combine recent chat and durable memory.
- Docker sandbox command construction avoids host path and Docker socket mounts.
- PTY session can keep a process alive and exchange text.
- Broker forwards OpenAI-compatible requests without exposing raw provider credentials to the sandbox.
- Telegram polling normalizes incoming group messages and calls the app handler.
- Runtime manager can ensure a per-chat sandbox and Codex session.
- Validation commands pass.

- [ ] **Step 4: Commit any final documentation adjustment**

Run:

```bash
git status --short
git add README.md docs/superpowers/plans/2026-04-30-telegram-codex-bridge-implementation.md
git commit -m "docs: finalize implementation plan"
```

Skip the commit if `git status --short` is empty.

---

## Known Risk Items To Resolve During Execution

- `portable-pty` restart is modeled as replacing a session object in the router. Keep that ownership boundary instead of mutating PTY reader and writer handles in place.
- The Dockerfile includes the current official Codex npm install path. If the Linux container package resolution changes, use the Linux release binary from the official `openai/codex` releases instead.
- The initial network policy blocks local CIDRs with container-local `iptables`. On Colima, verify that this blocks host gateway paths in practice. If it does not, add host-side Colima or Docker network firewall rules before considering the sandbox security complete.
- App startup wires polling after the core modules pass tests. Keep local pure-logic tests fast so Telegram failures are isolated to adapter-level checks.
