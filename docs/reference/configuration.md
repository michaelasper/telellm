# Configuration Reference

`telellm` loads TOML into `AppConfig`.

The example config is `config.example.toml`.

## `[telegram]`

| Field | Type | Required | Default In Example | Description |
| --- | --- | --- | --- | --- |
| `bot_token_env` | string | yes | `TELEGRAM_BOT_TOKEN` | Environment variable containing the Telegram bot token. |
| `bot_username` | string | yes | `telellm_bot` | Bot username used for mentions and addressed commands. |
| `allowed_chat_ids` | array of integers | no | `[-1001234567890]` | Chat allow-list. Empty means all chats are allowed. |

`bot_token_env` and `bot_username` must not be empty.

## `[telegram_ux]`

All fields are optional. The section controls Telegram-facing behavior for Codex turns.

| Field | Type | Default In Example | Description |
| --- | --- | --- | --- |
| `queue_ack_enabled` | boolean | `true` | Sends a short Telegram acknowledgement when a request is queued behind existing work. |
| `typing_indicator_enabled` | boolean | `true` | Sends Telegram `typing` chat actions while a turn is queued or running. |
| `typing_refresh_secs` | integer | `4` | Seconds between typing-action refreshes. Telegram typing indicators are transient. |
| `typing_for_queued_items` | boolean | `true` | Sends typing actions for queued items before their Codex turn starts. |
| `streaming_enabled` | boolean | `true` | Sends partial Codex output as edited Telegram messages when snapshots are available. |
| `streaming_mode` | string | `edit_message` | Streaming strategy. The current supported value is `edit_message`. |
| `streaming_update_interval_millis` | integer | `1500` | Minimum time between Telegram message edits. |
| `streaming_min_delta_chars` | integer | `80` | Minimum preview growth before another streaming edit is sent. |
| `streaming_max_chars` | integer | `3900` | Maximum characters shown in a streaming preview. Final responses still use normal chunking. |
| `formatting_mode` | string | `plain` | Response formatting mode. Valid values are `plain`, `markdown_v2`, and `html`. |
| `formatting_escape` | boolean | `true` | Escapes model output before applying Telegram parse mode. In `markdown_v2`, common `**bold**` spans are converted to Telegram bold while surrounding text stays escaped. |
| `formatting_fallback_to_plain` | boolean | `true` | Retries without parse mode when Telegram rejects formatted text. |

`typing_refresh_secs`, `streaming_update_interval_millis`, and `streaming_min_delta_chars` must be greater than zero. `streaming_max_chars` must be at least `256`.

## `[attachments]`

All fields are optional. The section controls Telegram photos and documents that are sent in addressed messages or DMs.

| Field | Type | Default In Example | Description |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | Downloads addressed Telegram photos and documents and imports them into the chat workspace. |
| `workspace_dir` | string | `telegram_uploads` | Relative directory under `/workspace` where imported attachments are stored. |
| `max_file_bytes` | integer | `20000000` | Maximum Telegram file size to download. Larger files are noted in prompt context but not imported. |

`workspace_dir` must be a non-empty relative path without parent directory components. `max_file_bytes` must be greater than zero.

## `[outputs]`

All fields are optional. The section controls files Codex creates in the chat workspace and mentions in its final answer.

| Field | Type | Default In Example | Description |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | Exports mentioned files from the chat workspace and sends them as Telegram documents. |
| `workspace_dir` | string | `telegram_outputs` | Relative directory under `/workspace` where Codex should write sendable files. |
| `max_file_bytes` | integer | `20000000` | Maximum generated file size to export and upload. |
| `max_files_per_response` | integer | `4` | Maximum number of generated files to send for one Codex response. |

`workspace_dir` must be a non-empty relative path without parent directory components. `max_file_bytes` and `max_files_per_response` must be greater than zero.

## `[audio]`

All fields are optional. The section controls inbound audio transcription and optional spoken replies.

| Field | Type | Default In Example | Description |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | Enables inbound Telegram voice/audio processing when an STT command is configured. |
| `replies_enabled` | boolean | `true` | Allows chats to receive synthesized spoken replies when TTS is configured and chat voice mode allows it. |
| `workspace_dir` | string | `telegram_audio` | Relative directory under `/workspace` where imported audio artifacts are stored. |
| `max_file_bytes` | integer | `20000000` | Maximum Telegram audio file size to download for transcription. |

`workspace_dir` must be a non-empty relative path without parent directory components. `max_file_bytes` must be greater than zero. STT and TTS command subsections are optional.

## `[audio.stt]`

Optional local speech-to-text command configuration.

| Field | Type | Default In Example | Description |
| --- | --- | --- | --- |
| `command` | string | `whisper-cli` | Local command used to transcribe downloaded audio. |
| `args` | array of strings | `["--model", "/models/ggml-base.en.bin", "--file", "{input}", "--output-txt", "{output}"]` | Arguments passed to the STT command. `{input}` is the host path to source audio and `{output}` is where transcript text should be written. |
| `timeout_secs` | integer | `120` | Maximum time to wait for the STT command. |

`command` must not be empty. `timeout_secs` must be greater than zero.

## `[audio.tts]`

Optional local text-to-speech command configuration.

| Field | Type | Default In Example | Description |
| --- | --- | --- | --- |
| `command` | string | `piper` | Local command used to synthesize spoken replies. |
| `args` | array of strings | `["--model", "/models/en_US-lessac-medium.onnx", "--output_file", "{output}"]` | Arguments passed to the TTS command. `{output}` is where generated audio should be written. |
| `stdin_text` | boolean | `true` | Writes the final Codex text to the TTS command's standard input. |
| `timeout_secs` | integer | `120` | Maximum time to wait for the TTS command. |
| `send_as` | string | `voice` | Telegram upload mode for generated speech. Valid values are `voice` and `audio`. |

`command` must not be empty. `timeout_secs` must be greater than zero.

## `[url_ingestion]`

All fields are optional. The section controls bounded fetching of URLs from addressed messages and DMs.

| Field | Type | Default In Example | Description |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | Enables URL fetching and readable-content extraction for addressed messages. |
| `workspace_dir` | string | `web_pages` | Relative directory under `/workspace` where fetched page snapshots are stored. |
| `max_urls_per_message` | integer | `3` | Maximum URLs to fetch from one Telegram message. |
| `max_fetch_bytes` | integer | `2000000` | Maximum response bytes to read for one URL. |
| `timeout_secs` | integer | `20` | Maximum time to wait for each URL fetch. |
| `user_agent` | string | `telellm/0.1` | User-Agent sent with URL fetch requests. |

`workspace_dir` must be a non-empty relative path without parent directory components. `user_agent` must not be empty. `max_urls_per_message`, `max_fetch_bytes`, and `timeout_secs` must be greater than zero.

## `[prompt]`

| Field | Type | Required | Default In Example | Description |
| --- | --- | --- | --- | --- |
| `system_prompt` | string | no | built-in Telegram assistant prompt | Instruction text rendered at the top of every Codex turn before long-term memory, recent chat, and the triggering message. |

`system_prompt` must not be empty after trimming whitespace. If `[prompt]` is omitted, the daemon uses the built-in Telegram group assistant prompt.

## `[storage]`

| Field | Type | Required | Default In Example | Description |
| --- | --- | --- | --- | --- |
| `sqlite_path` | path | yes | `data/telellm.sqlite` | Local SQLite database path for durable memory. |

The daemon creates the parent directory before connecting to SQLite.

## `[docker]`

| Field | Type | Required | Default In Example | Description |
| --- | --- | --- | --- | --- |
| `image` | string | yes | `telellm-sandbox:local` | Docker image used for group sandboxes. |
| `network` | string | yes | `telellm_public` | Docker network attached to group sandboxes. |
| `workspace_volume_prefix` | string | yes | `telellm_workspace` | Prefix for per-group named workspace volumes. |

All three fields must not be empty.

## `[codex]`

| Field | Type | Required | Default In Example | Description |
| --- | --- | --- | --- | --- |
| `command` | string | yes | `codex` | Command executed inside the sandbox container. |
| `args` | array of strings | no | `["exec", "--sandbox", "danger-full-access", "--skip-git-repo-check"]` | Extra arguments placed before `--model` and `--cd`. When the first argument is `exec`, the runtime uses non-interactive `codex exec` and returns only the final message. |
| `model` | string | yes | `gpt-5-codex` | Model passed to Codex with `--model`. In `chatgpt_oauth` mode, use a model available to the ChatGPT account. |
| `auth_mode` | string | no | `broker_api_key` | Codex credential strategy. Valid values are `broker_api_key` and `chatgpt_oauth`. |
| `auth_host_path` | path | when `auth_mode = "chatgpt_oauth"` | unset | Absolute host path to Codex CLI `auth.json`. |
| `env` | map of strings | no | `{}` | Parsed by config. It is not currently applied by the runtime. |

`command` and `model` must not be empty.

When `auth_mode` is `broker_api_key`, `[broker]` is required. When it is `chatgpt_oauth`, `auth_host_path` is required and must not be empty.

The runtime appends:

```text
--model <model> --cd /workspace
```

## `[broker]`

Required only when `codex.auth_mode` is `broker_api_key`.

| Field | Type | Required | Default In Example | Description |
| --- | --- | --- | --- | --- |
| `listen` | socket address | yes | `127.0.0.1:8189` | Host address where the broker listens. |
| `public_base_url` | URL string | yes | `http://host.docker.internal:8189/v1` | Base URL passed into sandboxes as `OPENAI_BASE_URL`. |
| `upstream_base_url` | URL string | yes | `https://api.openai.com/v1` | Upstream API base URL used by the host broker. |
| `upstream_api_key_env` | string | yes | `OPENAI_API_KEY` | Environment variable containing the upstream API key. |

`public_base_url`, `upstream_base_url`, and `upstream_api_key_env` must not be empty.

## `[limits]`

All limits are optional.

| Field | Default | Validation | Description |
| --- | ---: | --- | --- |
| `per_group_queue_depth` | `16` | greater than `0` | Bounded queue depth for each group's work queue. |
| `telegram_chunk_chars` | `3900` | at least `256` | Maximum byte length sent per Telegram message chunk. |
| `recent_buffer_messages` | `200` | coerced to at least `1` by the rolling buffer | In-memory recent messages retained per chat. |
| `codex_first_byte_timeout_secs` | `300` | greater than `0` | Maximum wait for the first PTY output byte. |
| `codex_inactivity_secs` | `600` | greater than `0` | Idle period after output before a turn is considered complete. |
| `codex_max_turn_secs` | `900` | greater than `0` | Maximum total PTY turn duration. |
| `codex_max_output_bytes` | `524288` | greater than `0` | Maximum PTY output bytes per turn. |
| `broker_max_requests_per_window` | `120` | greater than `0` | Per-token broker request count limit. |
| `broker_rate_limit_window_secs` | `60` | greater than `0` | Per-token broker request window. |
| `broker_max_concurrent_requests` | `4` | greater than `0` | Per-token concurrent broker request limit. |

## Required Environment Variables

The names are configurable.

| Default Name | Used By | Description |
| --- | --- | --- |
| `TELEGRAM_BOT_TOKEN` | Telegram adapter | Bot token from BotFather. |
| `OPENAI_API_KEY` | Host broker, only in `broker_api_key` mode | Upstream provider credential. This is not passed directly into the sandbox. |

In `chatgpt_oauth` mode, Codex uses the host auth file named by `codex.auth_host_path`; no upstream API key environment variable is required.
