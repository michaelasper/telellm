# Memory Model

`telellm` uses two different memory concepts: recent chat context and durable group memory.

They serve different needs and live in different places.

## Recent Chat

Recent chat is kept in an in-memory rolling buffer per chat.

It is populated when Telegram delivers messages to the daemon. Ambient messages can enter this buffer when Telegram bot privacy settings allow them to reach the bot.

Recent chat is:

- Process-local.
- Bounded by `limits.recent_buffer_messages`.
- Not stored in SQLite.
- Included in prompt context for addressed messages.

This gives Codex short-term context without turning the daemon into a permanent transcript archive.

## Durable Memory

Durable memory is stored in SQLite in the `memories` table.

Each record has:

- `id`
- `chat_id`
- optional `user_id`
- `kind`
- `content`
- `created_at`

Memory kinds supported by the code are:

- `person`
- `preference`
- `relationship`
- `group_norm`
- `running_joke`
- `personality`

The current `/remember <fact>` command stores the fact as `personality`.

## Prompt Context

For an addressed non-command message, the daemon renders a context packet containing:

1. The configured system prompt.
2. Durable group memories.
3. Recent chat lines.
4. The triggering message.

Codex receives this as prompt text. It does not receive write access to the host memory store.

## Forgetting

`/forget all` deletes all durable memory records for the current chat.

Targeted `/forget <query>` is acknowledged but not currently implemented. It does not remove matching records.

Clearing durable memory does not clear the group workspace volume. Clearing the workspace requires a runtime command with `--clear-workspace`.

## Current Implementation Boundary

The original design discusses automatic summarisation from ambient chat into durable memory. That summarisation path is not implemented in the current code.

Today, durable memory changes through explicit Telegram memory commands.
