# Telegram UX Streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add configurable Telegram typing indicators, edited-message streaming snapshots, and Telegram parse-mode support.

**Architecture:** Codex sessions emit optional snapshot events while preserving the existing final-turn API. The router consumes those events, manages typing loops, throttles Telegram edits, and sends the final authoritative response. Telegram formatting and UX behavior are configured through a new `[telegram_ux]` section.

**Tech Stack:** Rust 2024, Tokio, async-trait, teloxide, serde/TOML.

---

### Task 1: Configuration

**Files:**
- Modify: `src/config.rs`
- Modify: `config.example.toml`
- Modify: `docs/reference/configuration.md`

- [x] Add `TelegramUxConfig`, `TelegramStreamingMode`, and `TelegramFormatMode` with defaults.
- [x] Add `telegram_ux` to `AppConfig` with `#[serde(default)]`.
- [x] Validate positive timing and length values.
- [x] Update example config and reference docs.
- [x] Add config unit tests for defaults, parsing, and invalid zero values.

### Task 2: Telegram Sink Capabilities

**Files:**
- Modify: `src/bot/telegram.rs`
- Modify: router/app test fakes as needed.

- [x] Add `TelegramSendOptions` and `TelegramMessageHandle`.
- [x] Extend `TelegramSink` with formatted send, edit, streamed finalization, and chat-action methods.
- [x] Implement teloxide `send_chat_action(ChatAction::Typing)`, `send_message(...).parse_mode(...)`, and `edit_message_text(...).parse_mode(...)`.
- [x] Escape formatted output when configured and retry formatted send/edit as plain text when configured.

### Task 3: Codex Session Events

**Files:**
- Modify: `src/codex/session.rs`
- Modify: `src/codex/pty.rs`
- Modify: `src/codex/exec.rs`

- [x] Add `CodexTurnEvent::OutputSnapshot` and a sender type alias.
- [x] Add default `send_with_events` to `CodexSession`.
- [x] Emit cleaned PTY snapshots from `read_turn_output`.
- [x] Keep command exec final-output behavior via the default final snapshot.
- [x] Keep final `CodexTurn.output` behavior unchanged.

### Task 4: Router Orchestration

**Files:**
- Modify: `src/router.rs`
- Modify: `src/app.rs`
- Modify: `src/runtime.rs` tests as needed.

- [x] Add `Router::new_with_telegram_ux` and keep `Router::new` as default.
- [x] Start and stop queued/active typing loops.
- [x] Consume session event snapshots while the turn runs.
- [x] Send or edit streaming previews with interval and delta throttling.
- [x] Replace the streamed preview with the final response and suppress stale-generation updates.

### Task 5: Verification

**Files:**
- Modify: `docs/how-to/validate-and-troubleshoot.md` if behavior needs operator notes.

- [x] Run `cargo fmt`.
- [x] Run `./scripts/check.sh`.
- [x] Run focused tests for router/config/session changes during development.
- [x] Run `cargo audit`.
