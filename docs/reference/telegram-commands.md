# Telegram Command Reference

Commands are group-scoped. A command addressed to another bot, such as `/status@other_bot`, is ignored.

## Addressing

`telellm` handles:

| Input | Behaviour |
| --- | --- |
| Slash commands | Parsed by command name. |
| Mention of `@bot_username` | Enqueued as an addressed Codex prompt. |
| Reply to a bot message | Enqueued as an addressed Codex prompt. |
| Plain ambient group message | Stored only in the in-memory recent buffer when Telegram delivers it. |

## Commands

| Command | Arguments | Behaviour |
| --- | --- | --- |
| `/help` | none | Sends concise command help. |
| `/status` | none | Reports runtime state and generation. |
| `/reset` | optional `--clear-workspace` | Replaces the group's runtime session. With `--clear-workspace`, removes the workspace volume and rotates the broker token in `broker_api_key` mode. |
| `/restart` | none | Restarts the group Docker container and registers a fresh Codex session. |
| `/rebuild` | optional `--clear-workspace` | Recreates the group container from the configured image and rotates the broker token in `broker_api_key` mode. With `--clear-workspace`, removes the workspace volume. |
| `/memory` | none | Replies with durable memory records for the group. |
| `/remember` | text | Stores the text as a durable `personality` memory for the group. |
| `/forget` | `all` or text | `all` deletes all durable memory records for the group. Text targets are acknowledged but not currently deleted. |

## Runtime Status Values

| State | Description |
| --- | --- |
| `not started` | No runtime has been started for the group. |
| `starting` | A lifecycle operation is in progress. |
| `ready` | A Codex session is registered for the group. |
| `degraded` | Startup or replacement failed. The reason is included in the status text. |

## Queue Acknowledgement

When a group already has active or buffered work, and `telegram_ux.queue_ack_enabled` is enabled, the bot replies:

```text
Queued behind N existing request(s).
```

The queued work remains serialised through the per-group worker.

If `telegram_ux.typing_indicator_enabled` and `telegram_ux.typing_for_queued_items` are enabled, queued work can also refresh Telegram's typing indicator until its turn starts.

## Response Streaming And Formatting

When `telegram_ux.streaming_enabled` is enabled, addressed Codex prompts can produce a bot-owned streaming message that is edited with throttled output snapshots. The final Codex answer replaces the preview before any remaining chunks are sent.

Response formatting is controlled by `telegram_ux.formatting_mode`. The default `plain` mode sends text without Telegram parsing. `markdown_v2` and `html` use Telegram parse modes and can escape model output before sending when `telegram_ux.formatting_escape` is enabled.

## Command Parse Errors

Unknown commands and missing command arguments are logged by the daemon. They are not currently sent back as user-visible Telegram errors.
