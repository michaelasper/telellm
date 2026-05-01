# Telegram UX Streaming Design

## Goal

Add configurable Telegram typing indicators, throttled streaming updates, and Telegram parse-mode support for Codex responses.

## Scope

The first implementation supports one streaming strategy: send the first partial response as a normal Telegram message, then edit that message with throttled snapshots until the final Codex turn completes. The final turn remains authoritative and replaces the streamed preview. If the final response is longer than Telegram's message limit, the first chunk edits the streamed message and the remaining chunks are sent as follow-up messages.

Typing indicators are sent as Telegram `typing` chat actions while a turn is queued or running, controlled by refresh and enablement settings.

Formatting is configurable per daemon. The safe default is plain text. `markdown_v2` and `html` parse modes are supported for operators who want Telegram formatting. When enabled, formatting failures fall back to plain text if configured.

## Configuration

Add a `[telegram_ux]` section:

- `typing_indicator_enabled`: send Telegram typing actions while work is queued or running.
- `typing_refresh_secs`: interval for refreshing Telegram's transient typing indicator.
- `typing_for_queued_items`: send typing actions for queued items before their Codex turn starts.
- `streaming_enabled`: enable edited-message streaming snapshots.
- `streaming_mode`: initially `edit_message`.
- `streaming_update_interval_millis`: minimum time between edits.
- `streaming_min_delta_chars`: minimum output growth before an edit.
- `streaming_max_chars`: maximum characters shown in a streaming preview.
- `formatting_mode`: `plain`, `markdown_v2`, or `html`.
- `formatting_escape`: escape output before applying Telegram parse mode.
- `formatting_fallback_to_plain`: retry without parse mode when Telegram rejects formatted text.

## Architecture

`CodexSession` gains an optional event path. Existing callers can still use `send`, while the router calls `send_with_events` with an event channel. PTY mode emits cleaned output snapshots as bytes arrive. Command mode keeps final-output behavior and uses the default final snapshot because `codex exec --output-last-message` does not expose meaningful mid-turn answer output.

`TelegramSink` gains methods for chat actions, formatted sends, edits, and finishing streamed messages. The teloxide adapter maps these to `send_chat_action`, `send_message`, and `edit_message_text`.

The router owns orchestration: queued typing, active typing, event consumption, throttling, stale-generation suppression, and final delivery.

## Error Handling

Telegram action failures are logged and do not fail a Codex turn. Streaming edit failures are logged and do not prevent final response delivery. Final response send failures retain the current warning behavior. Parse-mode failures retry as plain text when `formatting_fallback_to_plain` is true.

## Testing

Add unit coverage for config defaults/validation, router typing, router streaming edits, stale-session suppression, and session event emission. Existing non-streaming behavior must remain unchanged.
