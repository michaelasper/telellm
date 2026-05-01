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

When a group already has active or buffered work, the bot replies:

```text
Queued behind N existing request(s).
```

The queued work remains serialised through the per-group worker.

## Command Parse Errors

Unknown commands and missing command arguments are logged by the daemon. They are not currently sent back as user-visible Telegram errors.
