# Ingestion And Recap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add local-command voice/audio transcription and spoken replies, stateless `/summarize`, and safe URL content ingestion for addressed Telegram messages.

**Architecture:** Host-side ingestion enriches `IncomingMessage` context before the existing per-chat router queues Codex work. Local STT/TTS commands live behind small audio adapters; URL fetching lives behind a bounded, SSRF-aware ingestor; `/summarize` builds a normal queued Codex prompt from the rolling chat buffer. The router sends final text first and optionally runs a post-turn audio reply hook for voice-triggered turns.

**Tech Stack:** Rust 2024, Tokio, async-trait, teloxide, reqwest, scraper, serde/TOML, sqlx SQLite.

---

## File Structure

- Modify `Cargo.toml`: add `scraper` for HTML title/body text extraction.
- Modify `src/lib.rs`: expose `audio`, `summary`, and `url` when each module is created.
- Modify `src/config.rs`: add `[audio]`, `[audio.stt]`, `[audio.tts]`, and `[url_ingestion]` config structs, defaults, and validation.
- Modify `config.example.toml`: add default audio and URL-ingestion sections.
- Modify `src/bot/message.rs`: add audio attachment kinds and context notes rendered into prompts.
- Modify `src/bot/telegram.rs`: normalize Telegram `voice` and `audio` messages and send generated voice/audio files.
- Modify `src/bot/command.rs`: add `/voice` and `/summarize` command parsing and command-menu entries.
- Modify `src/memory/store.rs`: add chat settings trait for per-chat voice reply mode.
- Modify `src/memory/sqlite.rs`: migrate and persist chat voice settings.
- Modify `src/memory/context.rs`: render context notes for transcripts and URL snapshots.
- Create `src/audio/mod.rs`: local STT/TTS command adapters, audio prompt context helpers, and router speech-reply trait.
- Create `src/summary.rs`: prompt builder for stateless catch-up summaries.
- Create `src/url/mod.rs`: URL detection, host safety checks, bounded fetch, HTML extraction, and snapshot generation.
- Modify `src/app.rs`: wire config, audio ingestion, URL ingestion, `/voice`, and `/summarize`.
- Modify `src/router.rs`: carry optional audio-reply metadata on work items and send synthesized audio after final text.
- Modify reference/how-to/explanation docs listed in the design spec.

---

### Task 1: Config And Dependency Setup

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/config.rs`
- Modify: `config.example.toml`
- Modify: `docs/reference/configuration.md`

- [ ] **Step 1: Add the HTML parsing dependency**

Run:

```bash
cargo add scraper
```

Expected: `Cargo.toml` gains a `scraper = "..."`
dependency and `Cargo.lock` is updated.

- [ ] **Step 2: Write failing config tests**

Add tests to the existing `#[cfg(test)] mod tests` in `src/config.rs`:

```rust
#[test]
fn audio_config_should_default_to_enabled_local_commands_absent() {
    let config = AppConfig::from_toml_str(MINIMAL_CONFIG).expect("config should parse");

    assert!(config.audio.enabled);
    assert!(config.audio.replies_enabled);
    assert_eq!(config.audio.workspace_dir, "telegram_audio");
    assert_eq!(config.audio.max_file_bytes, 20_000_000);
    assert!(config.audio.stt.is_none());
    assert!(config.audio.tts.is_none());
}

#[test]
fn audio_config_should_parse_local_commands() {
    let raw = format!(
        "{}\n{}",
        MINIMAL_CONFIG,
        r#"
[audio]
enabled = true
replies_enabled = true
workspace_dir = "audio"
max_file_bytes = 1234

[audio.stt]
command = "whisper-cli"
args = ["--file", "{input}", "--output-txt", "{output}"]
timeout_secs = 9

[audio.tts]
command = "piper"
args = ["--output_file", "{output}"]
stdin_text = true
timeout_secs = 8
send_as = "audio"
"#
    );

    let config = AppConfig::from_toml_str(&raw).expect("config should parse");

    assert_eq!(config.audio.workspace_dir, "audio");
    assert_eq!(config.audio.max_file_bytes, 1234);
    assert_eq!(config.audio.stt.as_ref().expect("stt").command, "whisper-cli");
    assert_eq!(config.audio.tts.as_ref().expect("tts").send_as, AudioSendAs::Audio);
}

#[test]
fn url_ingestion_config_should_default_to_enabled() {
    let config = AppConfig::from_toml_str(MINIMAL_CONFIG).expect("config should parse");

    assert!(config.url_ingestion.enabled);
    assert_eq!(config.url_ingestion.workspace_dir, "web_pages");
    assert_eq!(config.url_ingestion.max_urls_per_message, 3);
    assert_eq!(config.url_ingestion.max_fetch_bytes, 2_000_000);
    assert_eq!(config.url_ingestion.timeout_secs, 20);
    assert_eq!(config.url_ingestion.user_agent, "telellm/0.1");
}

#[test]
fn config_should_reject_invalid_audio_and_url_limits() {
    let cases = [
        (r#"[audio]
workspace_dir = "../audio"
"#, "audio.workspace_dir"),
        (r#"[audio]
max_file_bytes = 0
"#, "audio.max_file_bytes"),
        (r#"[audio.stt]
command = "whisper-cli"
timeout_secs = 0
"#, "audio.stt.timeout_secs"),
        (r#"[audio.tts]
command = "piper"
timeout_secs = 0
"#, "audio.tts.timeout_secs"),
        (r#"[url_ingestion]
workspace_dir = "/web"
"#, "url_ingestion.workspace_dir"),
        (r#"[url_ingestion]
max_urls_per_message = 0
"#, "url_ingestion.max_urls_per_message"),
        (r#"[url_ingestion]
max_fetch_bytes = 0
"#, "url_ingestion.max_fetch_bytes"),
        (r#"[url_ingestion]
timeout_secs = 0
"#, "url_ingestion.timeout_secs"),
    ];

    for (snippet, field) in cases {
        let raw = format!("{MINIMAL_CONFIG}\n{snippet}\n");
        let err = AppConfig::from_toml_str(&raw).expect_err("config should reject invalid value");
        assert!(
            err.to_string().contains(field),
            "expected `{field}` in `{err}`"
        );
    }
}
```

Run:

```bash
cargo test config::tests::audio_config_should_default_to_enabled_local_commands_absent config::tests::audio_config_should_parse_local_commands config::tests::url_ingestion_config_should_default_to_enabled config::tests::config_should_reject_invalid_audio_and_url_limits
```

Expected: FAIL with missing `audio`, `url_ingestion`, and `AudioSendAs` fields/types.

- [ ] **Step 3: Add config structs, defaults, and validation**

In `src/config.rs`, add `audio` and `url_ingestion` fields to `AppConfig`:

```rust
#[serde(default)]
pub audio: AudioConfig,
#[serde(default)]
pub url_ingestion: UrlIngestionConfig,
```

Add these structs and enum near the existing config structs:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct AudioConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub replies_enabled: bool,
    #[serde(default = "default_audio_workspace_dir")]
    pub workspace_dir: String,
    #[serde(default = "default_audio_max_file_bytes")]
    pub max_file_bytes: u64,
    #[serde(default)]
    pub stt: Option<AudioToolConfig>,
    #[serde(default)]
    pub tts: Option<AudioTtsConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AudioToolConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_audio_tool_timeout_secs")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AudioTtsConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub stdin_text: bool,
    #[serde(default = "default_audio_tool_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_audio_send_as")]
    pub send_as: AudioSendAs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioSendAs {
    Voice,
    Audio,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UrlIngestionConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_url_workspace_dir")]
    pub workspace_dir: String,
    #[serde(default = "default_max_urls_per_message")]
    pub max_urls_per_message: usize,
    #[serde(default = "default_max_fetch_bytes")]
    pub max_fetch_bytes: usize,
    #[serde(default = "default_url_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
}
```

Add `Default` impls, default functions, `validate_audio_config`, `validate_url_ingestion_config`, and call those validators from `AppConfig::validate`.

Use these defaults:

```rust
fn default_audio_workspace_dir() -> String {
    "telegram_audio".to_owned()
}

fn default_audio_max_file_bytes() -> u64 {
    20_000_000
}

fn default_audio_tool_timeout_secs() -> u64 {
    120
}

fn default_audio_send_as() -> AudioSendAs {
    AudioSendAs::Voice
}

fn default_url_workspace_dir() -> String {
    "web_pages".to_owned()
}

fn default_max_urls_per_message() -> usize {
    3
}

fn default_max_fetch_bytes() -> usize {
    2_000_000
}

fn default_url_timeout_secs() -> u64 {
    20
}

fn default_user_agent() -> String {
    "telellm/0.1".to_owned()
}
```

Validation rules:

```rust
fn validate_audio_config(audio: &AudioConfig) -> Result<(), ConfigError> {
    require_non_empty("audio.workspace_dir", &audio.workspace_dir)?;
    validate_relative_workspace_path("audio.workspace_dir", &audio.workspace_dir)?;
    if audio.max_file_bytes == 0 {
        return Err(ConfigError::InvalidValue {
            field: "audio.max_file_bytes",
            reason: "must be greater than zero",
        });
    }
    if let Some(stt) = &audio.stt {
        validate_audio_tool_config("audio.stt", stt)?;
    }
    if let Some(tts) = &audio.tts {
        require_non_empty("audio.tts.command", &tts.command)?;
        require_positive_u64("audio.tts.timeout_secs", tts.timeout_secs)?;
    }
    Ok(())
}

fn validate_audio_tool_config(
    prefix: &'static str,
    tool: &AudioToolConfig,
) -> Result<(), ConfigError> {
    require_non_empty(match prefix {
        "audio.stt" => "audio.stt.command",
        _ => "audio.tool.command",
    }, &tool.command)?;
    require_positive_u64(match prefix {
        "audio.stt" => "audio.stt.timeout_secs",
        _ => "audio.tool.timeout_secs",
    }, tool.timeout_secs)
}

fn validate_url_ingestion_config(urls: &UrlIngestionConfig) -> Result<(), ConfigError> {
    require_non_empty("url_ingestion.workspace_dir", &urls.workspace_dir)?;
    validate_relative_workspace_path("url_ingestion.workspace_dir", &urls.workspace_dir)?;
    require_non_empty("url_ingestion.user_agent", &urls.user_agent)?;
    if urls.max_urls_per_message == 0 {
        return Err(ConfigError::InvalidValue {
            field: "url_ingestion.max_urls_per_message",
            reason: "must be greater than zero",
        });
    }
    if urls.max_fetch_bytes == 0 {
        return Err(ConfigError::InvalidValue {
            field: "url_ingestion.max_fetch_bytes",
            reason: "must be greater than zero",
        });
    }
    require_positive_u64("url_ingestion.timeout_secs", urls.timeout_secs)
}
```

- [ ] **Step 4: Update config docs**

Add `[audio]`, `[audio.stt]`, `[audio.tts]`, and `[url_ingestion]` sections to `config.example.toml` using the approved spec values. Add matching tables to `docs/reference/configuration.md`.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo fmt
cargo test config::tests::audio_config_should_default_to_enabled_local_commands_absent config::tests::audio_config_should_parse_local_commands config::tests::url_ingestion_config_should_default_to_enabled config::tests::config_should_reject_invalid_audio_and_url_limits
```

Expected: PASS.

Commit:

```bash
git add Cargo.toml Cargo.lock src/config.rs config.example.toml docs/reference/configuration.md
git commit -m "feat: add ingestion config"
```

---

### Task 2: Telegram Audio Primitives

**Files:**
- Modify: `src/bot/message.rs`
- Modify: `src/bot/telegram.rs`
- Modify: `docs/reference/telegram-commands.md`

- [ ] **Step 1: Write failing message-kind tests**

Add tests in `src/bot/message.rs`:

```rust
#[test]
fn attachment_kind_should_identify_audio() {
    assert!(AttachmentKind::Voice.is_audio());
    assert!(AttachmentKind::Audio.is_audio());
    assert!(!AttachmentKind::Photo.is_audio());
    assert!(!AttachmentKind::Document.is_audio());
}

#[test]
fn attachment_kind_should_name_audio_defaults() {
    assert_eq!(AttachmentKind::Voice.as_str(), "voice");
    assert_eq!(AttachmentKind::Audio.as_str(), "audio");
    assert_eq!(AttachmentKind::Voice.default_file_name(), "voice.ogg");
    assert_eq!(AttachmentKind::Audio.default_file_name(), "audio");
}
```

Run:

```bash
cargo test bot::message::tests::attachment_kind_should_identify_audio bot::message::tests::attachment_kind_should_name_audio_defaults
```

Expected: FAIL because `Voice`, `Audio`, and `is_audio` do not exist.

- [ ] **Step 2: Extend attachment kinds**

Update `AttachmentKind`:

```rust
pub enum AttachmentKind {
    Photo,
    Document,
    Voice,
    Audio,
}

impl AttachmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Document => "document",
            Self::Voice => "voice",
            Self::Audio => "audio",
        }
    }

    pub fn default_file_name(self) -> &'static str {
        match self {
            Self::Photo => "photo.jpg",
            Self::Document => "document",
            Self::Voice => "voice.ogg",
            Self::Audio => "audio",
        }
    }

    pub fn is_audio(self) -> bool {
        matches!(self, Self::Voice | Self::Audio)
    }
}
```

- [ ] **Step 3: Add Telegram sink audio send API and tests**

Add to `src/bot/telegram.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramAudioSendKind {
    Voice,
    Audio,
}
```

Extend `TelegramSink`:

```rust
async fn send_audio_file(
    &self,
    _chat_id: ChatId,
    _path: &Path,
    _file_name: &str,
    _kind: TelegramAudioSendKind,
) -> Result<TelegramMessageHandle, TelegramError> {
    Err(TelegramError::AudioSend(
        "telegram audio uploads are not supported by this sink".to_owned(),
    ))
}
```

Add `AudioSend(String)` to `TelegramError`.

Add a unit test for the command menu after Task 3 adds `/voice`; in this task, add a lower-level test that `TelegramAudioSendKind` is copyable and comparable:

```rust
#[test]
fn telegram_audio_send_kind_should_compare() {
    assert_eq!(TelegramAudioSendKind::Voice, TelegramAudioSendKind::Voice);
    assert_ne!(TelegramAudioSendKind::Voice, TelegramAudioSendKind::Audio);
}
```

- [ ] **Step 4: Implement teloxide voice/audio normalization**

In `message_attachments`, check `message.voice()` and `message.audio()` between photo and document handling:

```rust
if let Some(voice) = message.voice() {
    return vec![IncomingAttachment::new(
        AttachmentKind::Voice,
        voice.file.id.0.clone(),
        voice.file.unique_id.0.clone(),
        Some("voice.ogg".to_owned()),
        voice.mime_type.as_ref().map(ToString::to_string),
        voice.file.size,
    )];
}

if let Some(audio) = message.audio() {
    return vec![IncomingAttachment::new(
        AttachmentKind::Audio,
        audio.file.id.0.clone(),
        audio.file.unique_id.0.clone(),
        audio.file_name.clone(),
        audio.mime_type.as_ref().map(ToString::to_string),
        audio.file.size,
    )];
}
```

Keep audio-like documents as `AttachmentKind::Document` for this task. Task 5 treats documents with `mime_type` starting with `audio/` as audio-like during audio ingestion.

- [ ] **Step 5: Implement teloxide audio sending**

Import `SendAudioSetters` and `SendVoiceSetters`. Implement `send_audio_file` for `TeloxideTelegramSink`:

```rust
async fn send_audio_file(
    &self,
    chat_id: ChatId,
    path: &Path,
    file_name: &str,
    kind: TelegramAudioSendKind,
) -> Result<TelegramMessageHandle, TelegramError> {
    let input = InputFile::file(path.to_path_buf()).file_name(file_name.to_owned());
    let message = match kind {
        TelegramAudioSendKind::Voice => self
            .bot
            .send_voice(TgChatId(chat_id.0), input)
            .await
            .map_err(|err| TelegramError::AudioSend(err.to_string()))?,
        TelegramAudioSendKind::Audio => self
            .bot
            .send_audio(TgChatId(chat_id.0), input)
            .await
            .map_err(|err| TelegramError::AudioSend(err.to_string()))?,
    };

    Ok(TelegramMessageHandle {
        chat_id,
        message_id: MessageId(message.id.0),
    })
}
```

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo fmt
cargo test bot::message::tests::attachment_kind_should_identify_audio bot::message::tests::attachment_kind_should_name_audio_defaults bot::telegram::tests::telegram_audio_send_kind_should_compare
```

Expected: PASS.

Commit:

```bash
git add src/bot/message.rs src/bot/telegram.rs docs/reference/telegram-commands.md
git commit -m "feat: add Telegram audio primitives"
```

---

### Task 3: Chat Voice Settings And Commands

**Files:**
- Modify: `src/bot/command.rs`
- Modify: `src/memory/store.rs`
- Modify: `src/memory/sqlite.rs`
- Modify: `src/app.rs`
- Modify: `docs/reference/telegram-commands.md`

- [ ] **Step 1: Write failing command parser tests**

Add to `src/bot/command.rs` tests:

```rust
#[test]
fn parse_should_capture_voice_commands() {
    assert_eq!(
        BotCommand::parse("/voice on", "telellm_bot").expect("parse"),
        Some(BotCommand::Voice {
            target: VoiceTarget::On,
        })
    );
    assert_eq!(
        BotCommand::parse("/voice off", "telellm_bot").expect("parse"),
        Some(BotCommand::Voice {
            target: VoiceTarget::Off,
        })
    );
    assert_eq!(
        BotCommand::parse("/voice status", "telellm_bot").expect("parse"),
        Some(BotCommand::Voice {
            target: VoiceTarget::Status,
        })
    );
    assert_eq!(
        BotCommand::parse("/voice", "telellm_bot").expect("parse"),
        Some(BotCommand::Voice {
            target: VoiceTarget::Status,
        })
    );
}

#[test]
fn parse_should_capture_summarize_commands() {
    assert_eq!(
        BotCommand::parse("/summarize", "telellm_bot").expect("parse"),
        Some(BotCommand::Summarize { focus: None })
    );
    assert_eq!(
        BotCommand::parse("/summarize the deploy", "telellm_bot").expect("parse"),
        Some(BotCommand::Summarize {
            focus: Some("the deploy".to_owned()),
        })
    );
}
```

Run:

```bash
cargo test bot::command::tests::parse_should_capture_voice_commands bot::command::tests::parse_should_capture_summarize_commands
```

Expected: FAIL with missing command variants.

- [ ] **Step 2: Add command variants**

Add:

```rust
Voice { target: VoiceTarget },
Summarize { focus: Option<String> },
```

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceTarget {
    On,
    Off,
    Status,
}
```

Parse:

```rust
"voice" => {
    let target = match rest.to_ascii_lowercase().as_str() {
        "" | "status" => VoiceTarget::Status,
        "on" => VoiceTarget::On,
        "off" => VoiceTarget::Off,
        _ => return Err(CommandParseError::Unknown(format!("voice {rest}"))),
    };
    Ok(Some(Self::Voice { target }))
}
"summarize" => Ok(Some(Self::Summarize {
    focus: (!rest.is_empty()).then(|| rest.to_owned()),
}))
```

Add command definitions:

```rust
CommandDefinition {
    command: "voice",
    description: "Manage spoken replies for this chat.",
},
CommandDefinition {
    command: "summarize",
    description: "Summarize recent chat for catching up.",
},
```

- [ ] **Step 3: Write failing chat-settings tests**

Add to `src/memory/sqlite.rs` tests:

```rust
#[tokio::test]
async fn voice_replies_should_default_to_enabled() {
    let store = SqliteMemoryStore::connect("sqlite::memory:")
        .await
        .expect("store should connect");

    assert!(
        store
            .voice_replies_enabled(ChatId(1))
            .await
            .expect("settings should load")
    );
}

#[tokio::test]
async fn voice_replies_should_persist_per_chat() {
    let store = SqliteMemoryStore::connect("sqlite::memory:")
        .await
        .expect("store should connect");

    store
        .set_voice_replies_enabled(ChatId(1), false)
        .await
        .expect("setting should save");

    assert!(
        !store
            .voice_replies_enabled(ChatId(1))
            .await
            .expect("setting should load")
    );
    assert!(
        store
            .voice_replies_enabled(ChatId(2))
            .await
            .expect("other chat should use default")
    );
}
```

Run:

```bash
cargo test memory::sqlite::tests::voice_replies_should_default_to_enabled memory::sqlite::tests::voice_replies_should_persist_per_chat
```

Expected: FAIL with missing trait methods.

- [ ] **Step 4: Add settings trait and SQLite implementation**

In `src/memory/store.rs`, add:

```rust
#[async_trait]
pub trait ChatSettingsStore: Send + Sync {
    async fn voice_replies_enabled(&self, chat_id: ChatId) -> Result<bool, MemoryStoreError>;
    async fn set_voice_replies_enabled(
        &self,
        chat_id: ChatId,
        enabled: bool,
    ) -> Result<(), MemoryStoreError>;
}
```

In `SqliteMemoryStore::migrate`, add:

```rust
sqlx::query(
    r#"
    CREATE TABLE IF NOT EXISTS chat_settings (
        chat_id INTEGER PRIMARY KEY,
        voice_replies_enabled INTEGER NOT NULL
    )
    "#,
)
.execute(&self.pool)
.await?;
```

Implement `ChatSettingsStore` for `SqliteMemoryStore`:

```rust
#[async_trait]
impl ChatSettingsStore for SqliteMemoryStore {
    async fn voice_replies_enabled(&self, chat_id: ChatId) -> Result<bool, MemoryStoreError> {
        let row = sqlx::query("SELECT voice_replies_enabled FROM chat_settings WHERE chat_id = ?1")
            .bind(chat_id.0)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row
            .map(|row| row.get::<i64, _>("voice_replies_enabled") != 0)
            .unwrap_or(true))
    }

    async fn set_voice_replies_enabled(
        &self,
        chat_id: ChatId,
        enabled: bool,
    ) -> Result<(), MemoryStoreError> {
        sqlx::query(
            r#"
            INSERT INTO chat_settings (chat_id, voice_replies_enabled)
            VALUES (?1, ?2)
            ON CONFLICT(chat_id) DO UPDATE SET voice_replies_enabled = excluded.voice_replies_enabled
            "#,
        )
        .bind(chat_id.0)
        .bind(if enabled { 1_i64 } else { 0_i64 })
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
```

- [ ] **Step 5: Wire `/voice` in AppCore**

Update `AppCore` generic bounds to require `M: MemoryStore + ChatSettingsStore + 'static`.

Add to `AppCoreConfig`:

```rust
pub audio: AudioConfig,
```

Handle voice commands:

```rust
BotCommand::Voice { target } => match target {
    VoiceTarget::On => {
        if !self.audio.replies_enabled {
            self.reply_text(
                message.chat_id,
                "Spoken replies are disabled by the daemon operator.",
            )
            .await?;
        } else {
            self.memory_store
                .set_voice_replies_enabled(message.chat_id, true)
                .await?;
            self.reply_text(message.chat_id, "Spoken replies are on for this chat.")
                .await?;
        }
    }
    VoiceTarget::Off => {
        self.memory_store
            .set_voice_replies_enabled(message.chat_id, false)
            .await?;
        self.reply_text(message.chat_id, "Spoken replies are off for this chat.")
            .await?;
    }
    VoiceTarget::Status => {
        let chat_enabled = self
            .memory_store
            .voice_replies_enabled(message.chat_id)
            .await?;
        self.reply_text(
            message.chat_id,
            format!(
                "Audio: {}. Spoken replies: {}. Chat voice mode: {}.",
                if self.audio.enabled { "enabled" } else { "disabled" },
                if self.audio.replies_enabled { "enabled" } else { "disabled" },
                if chat_enabled { "on" } else { "off" }
            ),
        )
        .await?;
    }
},
```

The STT/TTS availability fields are added to `/voice status` in Task 6 after both audio adapters are wired into app startup.

- [ ] **Step 6: Update app tests and verify**

Extend `FakeMemoryStore` in `src/app.rs` tests to implement `ChatSettingsStore` with an `AtomicBool` defaulting to true. Add tests:

```rust
#[tokio::test]
async fn voice_off_should_persist_chat_setting_without_runtime() {
    let (app, runtime, mut messages) = app_with_fakes().await;

    app.handle_message(incoming("/voice off"))
        .await
        .expect("voice command should work");

    assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 0);
    assert!(messages.recv().await.expect("reply").contains("off"));
    assert!(
        !app.memory_store
            .voice_replies_enabled(ChatId(1))
            .await
            .expect("setting should load")
    );
}
```

Run:

```bash
cargo fmt
cargo test bot::command::tests::parse_should_capture_voice_commands bot::command::tests::parse_should_capture_summarize_commands memory::sqlite::tests::voice_replies_should_default_to_enabled memory::sqlite::tests::voice_replies_should_persist_per_chat app::tests::voice_off_should_persist_chat_setting_without_runtime
```

Expected: PASS.

Commit:

```bash
git add src/bot/command.rs src/memory/store.rs src/memory/sqlite.rs src/app.rs docs/reference/telegram-commands.md
git commit -m "feat: add voice settings commands"
```

---

### Task 4: Local Audio Command Adapters

**Files:**
- Modify: `src/lib.rs`
- Create: `src/audio/mod.rs`
- Modify: `src/app.rs`

- [ ] **Step 1: Expose the audio module**

Add to `src/lib.rs`:

```rust
pub mod audio;
```

- [ ] **Step 2: Write failing audio adapter tests**

Create `src/audio/mod.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudioSendAs, AudioToolConfig, AudioTtsConfig};
    use tempfile::tempdir;

    #[tokio::test]
    async fn stt_command_should_write_transcript() {
        let dir = tempdir().expect("tempdir");
        let input = dir.path().join("input.ogg");
        tokio::fs::write(&input, b"fake audio").await.expect("write input");
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
        assert_eq!(tokio::fs::read(&speech.host_path).await.expect("read"), b"spoken reply");
        assert_eq!(speech.send_as, AudioSendAs::Voice);
    }
}
```

Run:

```bash
cargo test audio::tests::stt_command_should_write_transcript audio::tests::tts_command_should_write_audio_from_stdin
```

Expected: FAIL because the audio module types do not exist.

- [ ] **Step 3: Implement command adapters**

Add:

```rust
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
use tokio::{io::AsyncWriteExt, process::Command};

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
}

#[derive(Debug, Clone)]
pub struct LocalTts {
    config: AudioTtsConfig,
}

#[derive(Debug, Clone)]
pub struct AudioReplyRequest {
    pub chat_id: ChatId,
    pub trigger_message_id: MessageId,
    pub text: String,
}
```

Implement argument token expansion:

```rust
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
```

Implement `LocalStt::transcribe_to` and `LocalTts::synthesize_to` with `tokio::time::timeout`, `Command`, non-zero exit handling, output-file existence checks, and empty transcript rejection.

Use error variants:

```rust
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
```

Add:

```rust
impl AudioSendAs {
    pub fn telegram_kind(self) -> TelegramAudioSendKind {
        match self {
            Self::Voice => TelegramAudioSendKind::Voice,
            Self::Audio => TelegramAudioSendKind::Audio,
        }
    }
}
```

- [ ] **Step 4: Add audio reply trait**

Add to `src/audio/mod.rs`:

```rust
#[async_trait]
pub trait AudioReplySynthesizer: Send + Sync {
    async fn synthesize_reply(
        &self,
        request: AudioReplyRequest,
    ) -> Result<Option<SynthesizedAudio>, AudioError>;
}
```

This trait returns `Ok(None)` when TTS is unavailable or disabled for the request.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo fmt
cargo test audio::tests::stt_command_should_write_transcript audio::tests::tts_command_should_write_audio_from_stdin
```

Expected: PASS.

Commit:

```bash
git add src/lib.rs src/audio/mod.rs src/app.rs
git commit -m "feat: add local audio command adapters"
```

---

### Task 5: Inbound Audio Prompt Context

**Files:**
- Modify: `src/bot/message.rs`
- Modify: `src/memory/context.rs`
- Modify: `src/audio/mod.rs`
- Modify: `src/app.rs`

- [ ] **Step 1: Write failing context-note test**

Add `context_notes` to test messages in `src/memory/context.rs` and add:

```rust
#[test]
fn render_should_include_context_notes() {
    let mut trigger = message("@telellm_bot");
    trigger.context_notes.push(
        "Audio transcript from @telegram_audio/msg-1/1-voice.ogg: hello there".to_owned(),
    );
    let packet = ContextPacket {
        system_prompt: "Answer the trigger.".to_owned(),
        triggering_message: trigger,
        recent_messages: Vec::new(),
        memories: Vec::new(),
    };

    let rendered = packet.render();

    assert!(rendered.contains("Audio transcript from @telegram_audio/msg-1/1-voice.ogg"));
    assert!(rendered.contains("hello there"));
}
```

Run:

```bash
cargo test memory::context::tests::render_should_include_context_notes
```

Expected: FAIL because `context_notes` does not exist.

- [ ] **Step 2: Add context notes to message structs**

In `IncomingMessage` and `RepliedMessage`, add:

```rust
pub context_notes: Vec<String>,
```

Initialize it with `Vec::new()` everywhere messages are constructed in tests and `normalize_message`.

In `ContextPacket`, after `push_attachments`, render:

```rust
fn push_context_notes(output: &mut String, notes: &[String]) {
    for note in notes {
        output.push_str("  context: ");
        output.push_str(note);
        output.push('\n');
    }
}
```

Call `push_context_notes` for normal and replied messages.

- [ ] **Step 3: Write failing app audio ingestion test**

In `src/app.rs`, add:

```rust
#[tokio::test]
async fn addressed_voice_should_import_transcribe_and_render_context() {
    let (app, runtime, mut messages) =
        app_with_fakes_and_audio_stt("hello from the voice note").await;
    let mut message = incoming("@telellm_bot");
    message.attachments.push(IncomingAttachment::new(
        AttachmentKind::Voice,
        "voice-file-id".to_owned(),
        "voice-unique-id".to_owned(),
        Some("voice.ogg".to_owned()),
        Some("audio/ogg".to_owned()),
        15,
    ));

    app.handle_message(message)
        .await
        .expect("voice message should be handled");

    let prompt = messages.recv().await.expect("Codex prompt should be echoed");
    let imported = runtime.imported.lock().await;
    assert_eq!(imported[0].workspace_path, "telegram_audio/msg-1/1-voice.ogg");
    assert!(prompt.contains("hello from the voice note"));
    assert!(prompt.contains("@telegram_audio/msg-1/1-voice.ogg"));
}
```

Run:

```bash
cargo test app::tests::addressed_voice_should_import_transcribe_and_render_context
```

Expected: FAIL because `AppCore` does not prepare audio.

Add `app_with_fakes_and_audio_stt(transcript: &str)` in the test module. It should build `LocalStt` with `/bin/sh` and args that write the provided transcript into `{output}`, then pass that runner into `AppCore::new` through the new audio dependency field.

- [ ] **Step 4: Add audio ingestion service shape**

In `src/audio/mod.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAudio {
    pub workspace_path: String,
    pub transcript: Option<String>,
    pub skipped_reason: Option<String>,
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
        crate::app::sanitize_file_name(file_name)
    )
}
```

If `sanitize_file_name` is private in `app.rs`, move it to `src/bot/message.rs` as `sanitize_file_name` and update existing attachment path code to use it.

- [ ] **Step 5: Wire AppCore audio preparation**

Add to `AppCoreConfig`:

```rust
pub audio: AudioConfig,
```

Add optional STT runner to `AppCore`:

```rust
audio_stt: Option<Arc<crate::audio::LocalStt>>,
```

In `run`, construct it from `config.audio.stt.clone().map(LocalStt::new)`.

Before `prepare_attachments`, add:

```rust
self.prepare_audio(&mut message).await;
```

`prepare_audio` enforces `audio.enabled` and `audio.max_file_bytes`, downloads audio attachments using `telegram.download_file_to_path`, imports them into the runtime with `runtime.import_chat_attachment`, runs STT when configured, pushes transcript context notes, and sets `attachment.workspace_path`. It skips audio attachments in generic `prepare_attachment_list` by returning early for `crate::audio::is_audio_attachment(attachment)`.

Voice-only STT failure should return an `AppError::Audio(String)` with user-visible text:

```rust
Some("I could not transcribe that audio message. Check the daemon logs for details.")
```

When the message also has text, push a skipped note instead of failing.

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo fmt
cargo test memory::context::tests::render_should_include_context_notes app::tests::addressed_voice_should_import_transcribe_and_render_context
```

Expected: PASS.

Commit:

```bash
git add src/bot/message.rs src/memory/context.rs src/audio/mod.rs src/app.rs
git commit -m "feat: transcribe inbound audio"
```

---

### Task 6: Spoken Replies After Final Text

**Files:**
- Modify: `src/audio/mod.rs`
- Modify: `src/router.rs`
- Modify: `src/app.rs`
- Modify: `src/bot/telegram.rs`

- [ ] **Step 1: Write failing router audio reply test**

Add to `src/router.rs` tests:

```rust
#[tokio::test]
async fn router_should_send_spoken_reply_after_text_for_audio_turn() {
    let (telegram, mut messages, _typing, mut audio_files) = FakeTelegram::new();
    let synthesizer = Arc::new(FakeAudioReplySynthesizer::new("reply audio"));
    let router = Router::new_with_audio_replies(4, telegram, TelegramUxConfig::default(), synthesizer);
    router
        .register_session(ChatId(1), 1, Arc::new(FakeSession::default()))
        .await;

    router
        .enqueue(GroupWorkItem {
            chat_id: ChatId(1),
            prompt: "hello".to_owned(),
            audio_reply: Some(AudioReplyRequest {
                chat_id: ChatId(1),
                trigger_message_id: MessageId(7),
                text: String::new(),
            }),
        })
        .await
        .expect("enqueue should work");

    let text = messages.recv().await.expect("text response");
    let audio = audio_files.recv().await.expect("audio response");

    assert!(text.contains("reply: hello"));
    assert!(audio.contains("voice"));
}
```

Run:

```bash
cargo test router::tests::router_should_send_spoken_reply_after_text_for_audio_turn
```

Expected: FAIL because router audio reply hooks do not exist.

- [ ] **Step 2: Add audio metadata to work items**

Modify `GroupWorkItem`:

```rust
pub struct GroupWorkItem {
    pub chat_id: ChatId,
    pub prompt: String,
    pub audio_reply: Option<crate::audio::AudioReplyRequest>,
}
```

Update every existing `GroupWorkItem` construction to set `audio_reply: None`.

- [ ] **Step 3: Add router audio reply hook**

Add field:

```rust
audio_replies: Option<Arc<dyn crate::audio::AudioReplySynthesizer>>,
```

Add constructor:

```rust
pub fn new_with_audio_replies(
    queue_depth: usize,
    telegram: Arc<T>,
    telegram_ux: TelegramUxConfig,
    audio_replies: Arc<dyn crate::audio::AudioReplySynthesizer>,
) -> Self {
    Self {
        queue_depth: queue_depth.max(1),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        senders: Arc::new(Mutex::new(HashMap::new())),
        telegram,
        telegram_ux,
        audio_replies: Some(audio_replies),
    }
}
```

Make `Router::new_with_telegram_ux` set `audio_replies: None`.

Pass `audio_reply` into the worker. After successful final text delivery and generated-file sending, call:

```rust
Self::send_audio_reply(
    &telegram,
    audio_replies.clone(),
    chat_id,
    audio_reply,
    &turn.output,
)
.await;
```

Implement `send_audio_reply` to clone the request, replace `request.text` with `turn.output`, call the synthesizer, send with `telegram.send_audio_file`, and remove the temp file with `tokio::fs::remove_file`.

- [ ] **Step 4: Wire AppCore voice mode into work items**

When a triggering message included an audio attachment and STT succeeded or text exists, compute:

```rust
let audio_reply = if self.audio.enabled
    && self.audio.replies_enabled
    && message.attachments.iter().any(crate::audio::is_audio_attachment)
    && self.memory_store.voice_replies_enabled(message.chat_id).await?
{
    Some(crate::audio::AudioReplyRequest {
        chat_id: message.chat_id,
        trigger_message_id: message.message_id,
        text: String::new(),
    })
} else {
    None
};
```

Change `enqueue_text` to accept `audio_reply: Option<AudioReplyRequest>`.

In `run`, if `config.audio.tts` exists and replies are enabled, build an audio reply synthesizer with `config.audio.max_file_bytes` and use `Router::new_with_audio_replies`. The synthesizer must reject generated audio whose metadata length exceeds `audio.max_file_bytes` and return an `AudioError::OutputTooLarge { bytes, limit }`.

Update `/voice status` to include adapter availability:

```rust
format!(
    "Audio: {}. Spoken replies: {}. Chat voice mode: {}. STT: {}. TTS: {}.",
    if self.audio.enabled { "enabled" } else { "disabled" },
    if self.audio.replies_enabled { "enabled" } else { "disabled" },
    if chat_enabled { "on" } else { "off" },
    if self.audio_stt.is_some() { "available" } else { "unavailable" },
    if self.audio_tts.is_some() { "available" } else { "unavailable" },
)
```

- [ ] **Step 5: Update fake Telegram and synthesizer tests**

Extend `FakeTelegram` in router tests with an `audio_files` channel and implement `send_audio_file`. Add `FakeAudioReplySynthesizer` that writes bytes to a temp file and returns `SynthesizedAudio`.

Add a second test:

```rust
#[tokio::test]
async fn router_should_keep_text_when_audio_synthesis_fails() {
    let (telegram, mut messages, _typing, mut audio_files) = FakeTelegram::new();
    let synthesizer = Arc::new(FailingAudioReplySynthesizer);
    let router = Router::new_with_audio_replies(4, telegram, TelegramUxConfig::default(), synthesizer);
    router
        .register_session(ChatId(1), 1, Arc::new(FakeSession::default()))
        .await;

    router
        .enqueue(GroupWorkItem {
            chat_id: ChatId(1),
            prompt: "hello".to_owned(),
            audio_reply: Some(AudioReplyRequest {
                chat_id: ChatId(1),
                trigger_message_id: MessageId(7),
                text: String::new(),
            }),
        })
        .await
        .expect("enqueue should work");

    assert!(messages.recv().await.expect("text").contains("reply: hello"));
    assert!(matches!(
        audio_files.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}
```

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo fmt
cargo test router::tests::router_should_send_spoken_reply_after_text_for_audio_turn router::tests::router_should_keep_text_when_audio_synthesis_fails
```

Expected: PASS.

Commit:

```bash
git add src/audio/mod.rs src/router.rs src/app.rs src/bot/telegram.rs
git commit -m "feat: send spoken audio replies"
```

---

### Task 7: Stateless Summaries

**Files:**
- Modify: `src/lib.rs`
- Create: `src/summary.rs`
- Modify: `src/app.rs`
- Modify: `docs/reference/telegram-commands.md`

- [ ] **Step 1: Expose the summary module**

Add to `src/lib.rs`:

```rust
pub mod summary;
```

- [ ] **Step 2: Write failing summary prompt tests**

Create `src/summary.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bot::message::IncomingMessage,
        ids::{ChatId, MessageId, UserId},
    };

    fn msg(id: i32, text: &str) -> IncomingMessage {
        IncomingMessage {
            chat_id: ChatId(1),
            message_id: MessageId(id),
            from: Some(UserId(2)),
            from_name: Some("Mike".to_owned()),
            text: text.to_owned(),
            attachments: Vec::new(),
            context_notes: Vec::new(),
            reply_to_bot: false,
            reply_to: None,
            private_chat: false,
        }
    }

    #[test]
    fn summary_prompt_should_include_recent_chat_and_focus() {
        let prompt = build_summary_prompt(
            "System prompt.",
            &[msg(1, "first topic"), msg(2, "deploy failed")],
            Some("deploy"),
        )
        .expect("prompt");

        assert!(prompt.contains("catch-up recap"));
        assert!(prompt.contains("deploy failed"));
        assert!(prompt.contains("Focus: deploy"));
    }

    #[test]
    fn summary_prompt_should_reject_empty_recent_chat() {
        let err = build_summary_prompt("System prompt.", &[], None).expect_err("empty");
        assert_eq!(err, SummaryPromptError::NoRecentChat);
    }
}
```

Run:

```bash
cargo test summary::tests::summary_prompt_should_include_recent_chat_and_focus summary::tests::summary_prompt_should_reject_empty_recent_chat
```

Expected: FAIL because `summary` is empty.

- [ ] **Step 3: Implement summary prompt builder**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SummaryPromptError {
    #[error("no recent chat is available to summarize")]
    NoRecentChat,
}

pub fn build_summary_prompt(
    system_prompt: &str,
    recent_messages: &[IncomingMessage],
    focus: Option<&str>,
) -> Result<String, SummaryPromptError> {
    let useful: Vec<_> = recent_messages
        .iter()
        .filter(|message| !message.text.trim_start().starts_with("/summarize"))
        .filter(|message| !message.text.trim().is_empty() || !message.context_notes.is_empty())
        .collect();
    if useful.is_empty() {
        return Err(SummaryPromptError::NoRecentChat);
    }

    let mut prompt = String::new();
    prompt.push_str(system_prompt.trim());
    prompt.push_str("\n\nWrite a concise catch-up recap of the recent Telegram chat for someone who is joining late.");
    if let Some(focus) = focus.filter(|value| !value.trim().is_empty()) {
        prompt.push_str("\nFocus: ");
        prompt.push_str(focus.trim());
    }
    prompt.push_str("\n\nRecent chat:\n");
    for message in useful {
        let name = message.from_name.as_deref().unwrap_or("unknown");
        prompt.push_str("- ");
        prompt.push_str(name);
        prompt.push_str(": ");
        prompt.push_str(message.text.trim());
        prompt.push('\n');
        for note in &message.context_notes {
            prompt.push_str("  context: ");
            prompt.push_str(note);
            prompt.push('\n');
        }
    }
    Ok(prompt)
}
```

- [ ] **Step 4: Wire `/summarize` in AppCore**

In `handle_command`, add:

```rust
BotCommand::Summarize { focus } => {
    self.runtime.ensure_chat_runtime(message.chat_id).await?;
    let recent_messages = self.rolling.lock().await.recent_for_chat(message.chat_id);
    match crate::summary::build_summary_prompt(
        &self.system_prompt_for_turn(),
        &recent_messages,
        focus.as_deref(),
    ) {
        Ok(prompt) => self.enqueue_text(message.chat_id, prompt, None).await?,
        Err(crate::summary::SummaryPromptError::NoRecentChat) => {
            self.reply_text(
                message.chat_id,
                "There is not enough recent chat to summarize.",
            )
            .await?;
        }
    }
}
```

The command message itself is already pushed into rolling before command handling. `build_summary_prompt` ignores text that starts with `/summarize`.

- [ ] **Step 5: Add app test**

Add:

```rust
#[tokio::test]
async fn summarize_should_enqueue_recent_chat_without_memory_write() {
    let (app, runtime, mut messages) = app_with_fakes().await;

    app.handle_message(incoming("the deploy is blocked"))
        .await
        .expect("ambient should store");
    app.handle_message(incoming("/summarize deploy"))
        .await
        .expect("summarize should work");

    let prompt = messages.recv().await.expect("summary prompt should be echoed");
    assert!(prompt.contains("catch-up recap"));
    assert!(prompt.contains("the deploy is blocked"));
    assert_eq!(runtime.ensure_calls.load(Ordering::SeqCst), 1);
    assert_eq!(app.memory_store.remembered.load(Ordering::SeqCst), 0);
}
```

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo fmt
cargo test summary::tests::summary_prompt_should_include_recent_chat_and_focus summary::tests::summary_prompt_should_reject_empty_recent_chat app::tests::summarize_should_enqueue_recent_chat_without_memory_write
```

Expected: PASS.

Commit:

```bash
git add src/lib.rs src/summary.rs src/app.rs docs/reference/telegram-commands.md
git commit -m "feat: add stateless chat summaries"
```

---

### Task 8: URL Ingestion

**Files:**
- Modify: `src/lib.rs`
- Create: `src/url/mod.rs`
- Modify: `src/app.rs`
- Modify: `src/bot/message.rs`
- Modify: `src/memory/context.rs`

- [ ] **Step 1: Expose the URL module**

Add to `src/lib.rs`:

```rust
pub mod url;
```

- [ ] **Step 2: Write failing URL unit tests**

Create `src/url/mod.rs` with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UrlIngestionConfig;

    #[test]
    fn detect_urls_should_trim_common_trailing_punctuation() {
        let urls = detect_urls("Read https://example.com/path?x=1, then https://example.org.");
        assert_eq!(urls[0].as_str(), "https://example.com/path?x=1");
        assert_eq!(urls[1].as_str(), "https://example.org/");
    }

    #[test]
    fn safety_should_reject_private_and_local_hosts() {
        assert!(is_blocked_ip("127.0.0.1".parse().expect("ip")));
        assert!(is_blocked_ip("10.1.2.3".parse().expect("ip")));
        assert!(is_blocked_ip("172.16.0.1".parse().expect("ip")));
        assert!(is_blocked_ip("192.168.1.1".parse().expect("ip")));
        assert!(is_blocked_ip("169.254.1.1".parse().expect("ip")));
        assert!(!is_blocked_ip("93.184.216.34".parse().expect("ip")));
    }

    #[test]
    fn html_extraction_should_return_title_and_text() {
        let extracted = extract_readable_html(
            "https://example.com/page",
            br#"<html><head><title>Example</title></head><body><main><h1>Hello</h1><p>World</p></main></body></html>"#,
        )
        .expect("html should parse");

        assert_eq!(extracted.title.as_deref(), Some("Example"));
        assert!(extracted.markdown.contains("Hello"));
        assert!(extracted.markdown.contains("World"));
    }

    #[test]
    fn workspace_file_name_should_be_stable() {
        let url = reqwest::Url::parse("https://example.com/a/b?x=1").expect("url");
        assert_eq!(
            snapshot_workspace_path("web_pages", crate::ids::MessageId(5), 0, &url),
            "web_pages/msg-5/1-example.com.md"
        );
    }
}
```

Run:

```bash
cargo test url::tests::detect_urls_should_trim_common_trailing_punctuation url::tests::safety_should_reject_private_and_local_hosts url::tests::html_extraction_should_return_title_and_text url::tests::workspace_file_name_should_be_stable
```

Expected: FAIL because URL module functions do not exist.

- [ ] **Step 3: Implement URL detection, safety, and HTML extraction**

Add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPage {
    pub title: Option<String>,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlSnapshot {
    pub original_url: String,
    pub final_url: String,
    pub title: Option<String>,
    pub status: u16,
    pub content_type: Option<String>,
    pub bytes: usize,
    pub workspace_path: String,
    pub host_path: std::path::PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum UrlIngestionError {
    #[error("unsupported URL scheme")]
    UnsupportedScheme,
    #[error("URL host is blocked by safety policy")]
    BlockedHost,
    #[error("URL fetch failed: {0}")]
    Fetch(String),
    #[error("URL content exceeded {0} bytes")]
    TooLarge(usize),
    #[error("URL snapshot write failed: {0}")]
    SnapshotWrite(std::io::Error),
}
```

Implement:

- `detect_urls(text: &str) -> Vec<reqwest::Url>` by scanning whitespace tokens, trimming `.,);]}>` from the end, and accepting only `http`/`https`.
- `is_blocked_ip(ip: IpAddr) -> bool` using `is_loopback`, `is_private`, `is_link_local`, `is_multicast`, and IPv6 equivalents.
- `extract_readable_html(final_url: &str, bytes: &[u8]) -> Result<ExtractedPage, UrlIngestionError>` using `scraper::Html::parse_document`, `Selector::parse("title")`, and body/main text collection.
- `snapshot_workspace_path(workspace_dir, message_id, index, url)` using host-only file names.

- [ ] **Step 4: Add bounded fetcher**

Add `UrlIngestor`:

```rust
#[derive(Clone)]
pub struct UrlIngestor {
    config: UrlIngestionConfig,
    client: reqwest::Client,
}
```

Build the client with:

```rust
reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .timeout(Duration::from_secs(config.timeout_secs))
    .user_agent(config.user_agent.clone())
    .build()
```

Implement `fetch_snapshot(&self, message_id, index, url) -> Result<UrlSnapshot, UrlIngestionError>`:

1. Validate scheme.
2. Resolve `host:port` with `tokio::net::lookup_host`.
3. Reject the URL if any resolved IP is blocked.
4. Fetch without automatic redirects.
5. Follow at most five redirects manually, validating every hop before fetching.
6. Read chunks with `response.chunk().await`, stopping with `TooLarge(max_fetch_bytes)` when the accumulated bytes exceed the limit.
7. Extract HTML when `content-type` contains `text/html`; otherwise use UTF-8 lossy text for `text/*` and skip binary content with `UnsupportedScheme`.
8. Write a markdown snapshot to a temp host file containing URL, final URL, status, content type, title, and extracted text.

- [ ] **Step 5: Write failing app URL ingestion test**

Add to `src/app.rs` tests a fake ingestor by making `AppCore` accept an optional `Arc<dyn UrlContextProvider>`. Add a helper named `app_with_fakes_and_url_snapshot` that installs a fake provider returning one `UrlSnapshot` with a temp host file containing `# Example\n\nReadable page text`. The test shape:

```rust
#[tokio::test]
async fn addressed_message_should_ingest_url_context() {
    let (app, runtime, mut messages) = app_with_fakes_and_url_snapshot(
        "https://example.com/",
        "web_pages/msg-1/1-example.com.md",
        "# Example\n\nReadable page text",
    ).await;

    app.handle_message(incoming("@telellm_bot read https://example.com/"))
        .await
        .expect("message should be handled");

    let prompt = messages.recv().await.expect("prompt");
    let imported = runtime.imported.lock().await;
    assert_eq!(imported[0].workspace_path, "web_pages/msg-1/1-example.com.md");
    assert!(prompt.contains("URL context"));
    assert!(prompt.contains("@web_pages/msg-1/1-example.com.md"));
}
```

Run:

```bash
cargo test app::tests::addressed_message_should_ingest_url_context
```

Expected: FAIL because app URL preparation does not exist.

- [ ] **Step 6: Wire URL ingestion into AppCore**

Add an app-level trait in `src/url/mod.rs`:

```rust
#[async_trait]
pub trait UrlContextProvider: Send + Sync {
    async fn snapshots_for_text(
        &self,
        message_id: MessageId,
        text: &str,
    ) -> Vec<Result<UrlSnapshot, UrlIngestionError>>;
}
```

Implement it for `UrlIngestor`.

In `AppCore`, add:

```rust
url_ingestor: Option<Arc<dyn crate::url::UrlContextProvider>>,
```

After runtime ensure and before rolling push, call `prepare_urls(&mut message)`. For each successful snapshot, import the host snapshot into the workspace with `runtime.import_chat_attachment`, then push a context note:

```rust
format!(
    "URL context: {} ({}) saved at @{}",
    snapshot.title.as_deref().unwrap_or("untitled page"),
    snapshot.final_url,
    snapshot.workspace_path
)
```

For each failure, push:

```rust
format!("URL skipped: {url} ({reason})")
```

Remove temp host snapshot files after import.

- [ ] **Step 7: Verify and commit**

Run:

```bash
cargo fmt
cargo test url::tests::detect_urls_should_trim_common_trailing_punctuation url::tests::safety_should_reject_private_and_local_hosts url::tests::html_extraction_should_return_title_and_text url::tests::workspace_file_name_should_be_stable app::tests::addressed_message_should_ingest_url_context
```

Expected: PASS.

Commit:

```bash
git add src/lib.rs src/url/mod.rs src/app.rs src/bot/message.rs src/memory/context.rs
git commit -m "feat: ingest URL context"
```

---

### Task 9: Documentation And Verification

**Files:**
- Modify: `docs/reference/configuration.md`
- Modify: `docs/reference/telegram-commands.md`
- Modify: `docs/how-to/send-attachments.md`
- Create: `docs/how-to/use-voice-and-audio.md`
- Modify: `docs/explanation/message-lifecycle.md`
- Modify: `docs/explanation/runtime-and-security.md`
- Modify: `docs/how-to/validate-and-troubleshoot.md`

- [ ] **Step 1: Update operator docs**

Document:

- `[audio]`, `[audio.stt]`, `[audio.tts]`, and `[url_ingestion]` config.
- `/voice on`, `/voice off`, `/voice status`, `/summarize`, and `/summarize <topic>`.
- Voice/audio input flow, transcript behavior, and spoken reply behavior.
- URL ingestion limits, SSRF restrictions, redirects, and skipped URL notes.
- Local command examples for `whisper-cli` and `piper`.

- [ ] **Step 2: Update validation docs**

In `docs/how-to/validate-and-troubleshoot.md`, add checks for:

```bash
whisper-cli --help
piper --help
cargo run -- doctor --config config.toml
```

Explain that `/voice status` is the authoritative runtime availability check for this milestone.

- [ ] **Step 3: Run full verification**

Run:

```bash
cargo fmt
./scripts/check.sh
cargo audit
```

Expected: PASS for formatting, tests, Clippy, and audit.

- [ ] **Step 4: Commit docs and verification cleanup**

Commit:

```bash
git add docs/reference/configuration.md docs/reference/telegram-commands.md docs/how-to/send-attachments.md docs/how-to/use-voice-and-audio.md docs/explanation/message-lifecycle.md docs/explanation/runtime-and-security.md docs/how-to/validate-and-troubleshoot.md
git commit -m "docs: document ingestion and recap features"
```

---

## Final Acceptance

- `config.example.toml` includes audio and URL-ingestion defaults.
- `/voice status` works without starting Codex and reports global audio, global replies, chat mode, STT availability, and TTS availability.
- Voice/audio messages can be transcribed into prompt context.
- Voice/audio-triggered Codex turns send text first, then audio when globally enabled, chat-enabled, and TTS-available.
- `/voice off` suppresses spoken replies for that chat; `/voice on` re-enables them.
- `/summarize` queues a catch-up recap from recent rolling chat and writes no durable memory.
- Addressed URLs produce prompt context notes pointing at workspace snapshots.
- URL ingestion rejects local/private/link-local/multicast targets and validates redirect hops.
- STT, TTS, URL fetch, and URL parse failures do not break normal text responses except voice-only STT failure, which returns a concise transcription error.
- `./scripts/check.sh` and `cargo audit` pass.
