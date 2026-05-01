# Message Lifecycle

`telellm` treats Telegram messages differently depending on whether the bot is being addressed.

This distinction keeps normal group chat quiet while still letting the bot catch up when Telegram delivers ambient messages.

## Ambient Messages

An ambient message is plain group chat that is not a command, does not mention the bot, and is not a reply to the bot.

When Telegram delivers an ambient message, the daemon stores it in the in-memory rolling buffer for that chat. It does not start Codex and does not reply.

The rolling buffer is short-lived process memory. It is useful for context, not for permanent transcript storage.

## Addressed Messages

A message is addressed when it:

- Mentions `@bot_username`.
- Replies to a bot message.
- Uses a supported slash command.

For addressed non-command messages, the daemon builds a context packet:

- The configured system prompt.
- Durable group memory.
- Recent chat from the rolling buffer.
- The triggering message.

That rendered packet is sent to Codex as the prompt.

## Commands

Commands are parsed before addressed prompt handling.

Runtime and memory commands execute on the host side. They do not ask Codex to perform the operation.

This is why `/remember`, `/forget all`, `/reset`, `/restart`, and `/rebuild` can work without sending an instruction to the group Codex session.

## Queueing

Each group has one bounded work queue for the active session generation.

The router serialises Codex turns for a group because the PTY is an interactive process. Concurrent writes to that process would corrupt the conversation.

Different groups can start or run independently. The runtime manager also uses per-chat lifecycle locks so two startup or replacement operations for the same group do not race.

## Stale Generations

Runtime replacement creates a new generation. Router workers check the current generation before and after a Codex turn.

If a response belongs to a stale generation, the daemon suppresses it. This prevents an old reset or rebuild response from appearing after a newer session has taken over.

## Output Completion

The PTY supervisor does not receive structured "turn complete" events from Codex. It reads bytes until one of these conditions happens:

- First-byte timeout before any output.
- Inactivity timeout after some output.
- Maximum turn timeout.
- Maximum output byte limit.
- PTY closes.

While the PTY produces output, the router can stream cleaned cumulative snapshots to Telegram by editing one bot-owned message. Snapshots are throttled by `telegram_ux.streaming_update_interval_millis` and `telegram_ux.streaming_min_delta_chars`.

The final cleaned output is authoritative. If a streamed preview exists, the router edits it with the first final chunk and sends any remaining chunks as follow-up messages.

The router can also refresh Telegram `typing` chat actions while work is queued or running. These indicators are transient Telegram UI state; they do not create messages and they stop when the bot sends a message.
