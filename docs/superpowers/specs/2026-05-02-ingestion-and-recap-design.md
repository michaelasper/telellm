# Ingestion And Recap Design

## Goal

Add three user-facing feature areas to `telellm`:

- Voice and audio conversations can transcribe incoming audio as Codex prompt input and optionally return spoken replies through a configurable local TTS command.
- `/summarize` can produce a catch-up recap from recent chat without writing durable memory.
- Addressed messages that contain URLs can fetch and parse page content so Codex receives better context than the raw link alone.

These features should fit the existing Telegram-to-Codex lifecycle: host-owned ingestion prepares useful context, the per-chat router serializes Codex work, and Telegram delivery remains text-first with optional audio as an additive output.

## Scope

The first implementation supports local command adapters for speech-to-text and text-to-speech. Operators can point `telellm` at tools such as `whisper-cli`, `piper`, macOS `say` wrapped by a script, or another command-line adapter. Built-in provider-specific STT or TTS APIs are out of scope for this milestone, but the config and module boundaries should leave room for provider adapters later.

Audio replies are enabled by default globally, but they require a working TTS command and a per-chat voice mode that has not been turned off. A voice or audio message can automatically trigger a spoken reply when all controls allow it. Users can change the chat mode with `/voice on`, `/voice off`, and inspect it with `/voice status`.

`/summarize` is stateless. It reads the existing rolling recent-message buffer for the chat, sends a summarization prompt through the group's normal Codex queue, and returns the result as a Telegram text message. It does not insert or update SQLite durable memories.

URL ingestion applies only to addressed messages and DMs. Ambient group messages can still enter recent chat as they do today, but their URLs are not fetched.

## Architecture

Add three focused modules:

| Module | Responsibility |
| --- | --- |
| `audio` | Downloaded audio import, local STT command execution, local TTS command execution, and audio-specific prompt notes. |
| `url` | URL detection, safety checks, bounded fetching, readable-content extraction, and workspace snapshots. |
| `summary` | `/summarize` prompt construction from recent rolling chat. |

The existing `app` layer remains the coordinator. It normalizes Telegram messages, handles commands, prepares attachment-like context from audio and URLs, then enqueues Codex work through the router. The router remains responsible for serialized execution, stale-generation suppression, streaming text updates, final text delivery, and generated-file export.

Per-chat voice mode should be durable across daemon restarts. Store it in SQLite using a small chat settings table rather than the memory table, because it is operational state, not prompt memory.

The local command boundary should be represented by small adapter structs that accept resolved paths and return structured results. They should not know about Telegram, Codex, or SQLite.

## Commands

Add these commands to Telegram command parsing and startup command registration:

| Command | Arguments | Behaviour |
| --- | --- | --- |
| `/voice` | `on` | Enables automatic spoken replies for the current chat when global audio replies and TTS are available. |
| `/voice` | `off` | Disables automatic spoken replies for the current chat. Transcription can still work if audio ingestion is enabled. |
| `/voice` | `status` or none | Reports global audio enablement, global reply enablement, chat voice mode, STT availability, and TTS availability. |
| `/summarize` | none | Summarizes recent rolling chat for someone catching up. |
| `/summarize` | text | Summarizes recent rolling chat with focus on the provided topic, person, or question. |

If `audio.replies_enabled = false`, `/voice on` should not enable chat voice mode. It should reply that spoken replies are disabled by the operator.

## Configuration

Add an `[audio]` section:

```toml
[audio]
enabled = true
replies_enabled = true
workspace_dir = "telegram_audio"
max_file_bytes = 20000000

[audio.stt]
command = "whisper-cli"
args = ["--model", "/models/ggml-base.en.bin", "--file", "{input}", "--output-txt", "{output}"]
timeout_secs = 120

[audio.tts]
command = "piper"
args = ["--model", "/models/en_US-lessac-medium.onnx", "--output_file", "{output}"]
stdin_text = true
timeout_secs = 120
send_as = "voice"
```

`audio.enabled` controls inbound audio processing. When it is false, audio files should not be transcribed.

`audio.replies_enabled` controls whether any chat may receive synthesized spoken replies. It defaults to true, but a missing or invalid TTS command still makes spoken replies unavailable.

`audio.workspace_dir` must be a non-empty relative path without parent components. `audio.max_file_bytes` must be greater than zero. STT and TTS commands are optional so operators can enable only transcription, only status visibility, or both transcription and spoken replies.

Supported placeholders:

| Placeholder | Meaning |
| --- | --- |
| `{input}` | Host path to the downloaded source audio file for STT. |
| `{output}` | Host path where the command should write transcript text or generated audio. |

For TTS, `stdin_text = true` means `telellm` writes the final Codex text to the command's standard input. A future adapter can add a `{text}` placeholder if needed, but the initial design should avoid passing long model output through command-line arguments.

`send_as` supports `voice` and `audio`. `voice` should use Telegram's voice-message API when possible. `audio` should send a normal Telegram audio file.

Add a `[url_ingestion]` section:

```toml
[url_ingestion]
enabled = true
workspace_dir = "web_pages"
max_urls_per_message = 3
max_fetch_bytes = 2000000
timeout_secs = 20
user_agent = "telellm/0.1"
```

`url_ingestion.workspace_dir` must be a non-empty relative path without parent components. Limits must be greater than zero.

## Voice And Audio Input

When an addressed Telegram message or DM contains a voice note, audio file, or audio-like document:

1. Enforce `audio.enabled`, `audio.max_file_bytes`, and existing attachment safety rules.
2. Download the file on the host.
3. Copy it into the chat workspace under `/workspace/<audio.workspace_dir>/msg-<message-id>/`.
4. Run the configured STT command with resolved `{input}` and `{output}` paths.
5. Read the transcript from the output path.
6. Add the transcript to prompt context as the user's message content.
7. Add the original workspace audio path and transcription metadata to prompt context.

If the user sent a voice-only addressed message and STT fails, reply with a short Telegram error explaining that transcription failed. If the message also contains text, continue with the text and include a skipped-audio note in prompt context.

The transcript should not be written to durable memory automatically.

## Spoken Replies

After Codex produces the final text answer:

1. Check that the triggering message included voice or audio.
2. Check `audio.enabled`, `audio.replies_enabled`, chat voice mode, and TTS availability.
3. Run the configured TTS command with the final text.
4. Enforce the generated audio size limit before upload.
5. Send the normal text response first.
6. Send the generated audio as `voice` or `audio` according to `audio.tts.send_as`.

Text delivery remains authoritative. TTS failure must not suppress the text answer. Log TTS failures and, if useful, append a short Telegram note only when the failure is user-actionable.

The default per-chat voice mode should be on when no stored setting exists, so voice and audio messages receive spoken replies automatically when global config and TTS availability allow it. `/voice off` is the per-chat opt-out, and `/voice on` re-enables the default behavior.

## Summarize

`/summarize` builds a Codex prompt from the chat's rolling recent buffer. It should include:

- The configured system prompt.
- A short instruction to produce a catch-up recap.
- Recent chat lines already retained by the rolling buffer.
- An optional user-provided focus string from `/summarize <topic>`.

The command uses the normal per-chat queue, so it does not race active Codex turns. It should not include durable memory unless the existing prompt renderer requires it for consistency. The summary result is sent as a normal text response and is not stored in SQLite.

If there is no useful recent chat, reply with a short message saying there is not enough recent chat to summarize.

## URL Ingestion

When an addressed message contains URLs:

1. Detect HTTP and HTTPS URLs in the message text and captions.
2. Ignore unsupported schemes.
3. Enforce `url_ingestion.enabled` and `max_urls_per_message`.
4. Resolve and validate hosts before fetching.
5. Reject loopback, private, link-local, multicast, and local IPv6 targets to avoid SSRF against the host or LAN.
6. Fetch with configured timeout, byte limit, and user agent.
7. Convert HTML to readable markdown or text.
8. Store snapshots under `/workspace/<url_ingestion.workspace_dir>/msg-<message-id>/`.
9. Add title, final URL, status, content type, byte count, and workspace path to prompt context.

Redirects are allowed only if each hop passes the same host/IP safety checks. Fetch failures, unsupported content types, and oversized pages should be represented as skipped URL notes in prompt context.

URL snapshots are context artifacts, not generated outputs. They should not be automatically sent back to Telegram unless Codex later references them under the configured outputs directory.

## Error Handling

Local command failures should include enough detail in logs to diagnose missing binaries, non-zero exits, timeouts, and missing output files. User-facing errors should stay short.

Failure handling:

| Failure | Behaviour |
| --- | --- |
| Audio too large | Skip transcription and include or send a size-limit note. |
| STT command unavailable | `/voice status` reports unavailable; voice-only prompts receive a transcription error. |
| STT timeout | Kill the child process and treat as transcription failure. |
| STT output missing or empty | Treat as transcription failure. |
| TTS command unavailable | `/voice status` reports unavailable; text responses continue. |
| TTS timeout | Kill the child process, log, and keep the text response. |
| URL blocked by safety policy | Include a skipped URL note in prompt context. |
| URL fetch timeout or byte limit | Include a skipped or truncated URL note in prompt context. |
| URL parse failure | Store raw text when safe and useful, otherwise include a parse-failure note. |

## Testing

Add focused coverage for:

- Config defaults and validation for `[audio]`, `[audio.stt]`, `[audio.tts]`, and `[url_ingestion]`.
- Command parsing for `/voice on`, `/voice off`, `/voice status`, `/summarize`, and `/summarize <topic>`.
- SQLite chat setting persistence for voice mode.
- STT and TTS local command adapters using small test fixtures.
- Audio prompt context when transcription succeeds, fails, or is skipped for size.
- Spoken reply behavior when TTS succeeds, fails, or is globally disabled.
- `/summarize` using recent chat without writing durable memory.
- URL detection, invalid schemes, URL count limits, redirect safety, private-address rejection, byte limits, and readable HTML extraction.
- App or router integration tests proving URL snapshots and transcript paths are included in Codex prompt context.

Real `whisper-cli` or `piper` tests should be ignored integration tests or operator documentation, not required CI tests.

## Rollout

Update:

- `config.example.toml`
- `docs/reference/configuration.md`
- `docs/reference/telegram-commands.md`
- `docs/how-to/send-attachments.md` or a new audio how-to
- `docs/explanation/message-lifecycle.md`
- `docs/explanation/runtime-and-security.md`

The feature should degrade gracefully. A default config can enable the sections, but missing STT or TTS commands should make capabilities unavailable rather than causing daemon startup failure. Operators should be able to discover that state through `/voice status` and `doctor`.

For this milestone, `/voice status` must report STT and TTS availability clearly. `doctor` checks for configured STT/TTS commands and URL-ingestion safety settings can land in a follow-up implementation phase.
