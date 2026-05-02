# Message Lifecycle

`telellm` treats Telegram messages differently depending on whether the bot is being addressed.

This distinction keeps normal group chat quiet while still letting the bot catch up when Telegram delivers ambient messages.

## Ambient Messages

An ambient message is group chat that is not a command, does not mention the bot, and is not a reply to the bot.

When Telegram delivers an ambient message, the daemon stores it in the in-memory rolling buffer for that chat. It does not start Codex and does not reply.

Unaddressed attachments, voice/audio messages, and URLs are not downloaded, transcribed, or fetched. They become input only when Telegram delivers them as part of an addressed message or reply context.

The rolling buffer is short-lived process memory. It is useful for context, not for permanent transcript storage.

## Addressed Messages

A message is addressed when it:

- Mentions `@bot_username`.
- Replies to a bot message.
- Uses a supported slash command.

When Telegram includes a replied-to text message in the update, `telellm` preserves that snapshot and renders it as explicit reply context. This is especially important for replies to the bot's own messages, because the daemon may not otherwise receive its outgoing Telegram messages back through polling.

For addressed non-command messages, the daemon builds a context packet:

- The configured system prompt.
- Durable group memory.
- Recent chat from the rolling buffer.
- The replied-to message, when Telegram provided one.
- The triggering message.

If the addressed message or its replied-to message includes Telegram photos or documents, the daemon downloads each attachment on the host, copies it into the chat sandbox under `/workspace/<attachments.workspace_dir>/msg-<message-id>/`, and renders the workspace path in the context packet as an `@...` file reference. Files that are too large or fail to download are rendered as skipped attachments with the reason.

If the addressed message or its replied-to message includes Telegram voice or audio, the daemon downloads the file on the host, copies it into the chat sandbox under `/workspace/<audio.workspace_dir>/msg-<message-id>/`, runs the configured STT command, and renders the transcript as a context note. If transcription fails while the triggering message has other usable text context, the packet includes a skipped transcript note and the Codex turn still runs. If the triggering message is voice-only and transcription fails, the bot returns a concise transcription error instead of sending an empty prompt to Codex.

If the triggering text contains HTTP or HTTPS URLs and URL ingestion is enabled, the daemon fetches bounded readable snapshots on the host, imports successful snapshots into `/workspace/<url_ingestion.workspace_dir>/msg-<message-id>/`, and renders context notes pointing at the `@...` snapshot paths. Fetch, redirect, parse, content-type, and safety failures are rendered as skipped URL notes and do not stop the Codex turn.

That rendered packet is sent to Codex as the prompt.

## Commands

Commands are parsed before addressed prompt handling.

Runtime, memory, and voice-mode commands execute on the host side. They do not ask Codex to perform the operation.

This is why `/remember`, `/forget all`, `/voice status`, `/voice on`, `/voice off`, `/reset`, `/restart`, and `/rebuild` can work without sending an instruction to the group Codex session.

`/summarize` is different. It builds a stateless catch-up recap prompt from the system prompt and recent in-memory rolling chat, then queues that prompt for Codex. It reads and writes no durable memory records.

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

If the final output mentions files under `@<outputs.workspace_dir>/...`, the runtime validates each path, copies matching files out of the chat sandbox, and the router sends them as Telegram documents after the final text response. Failed exports are logged and also appended to the text response as short attachment notes.

For voice/audio-triggered turns, the router can synthesize a spoken reply after the final text response when global audio is enabled, spoken replies are globally enabled, the chat voice mode is on, and TTS is available. TTS and Telegram audio-upload failures are logged; they do not change the already-sent text response.

The router can also refresh Telegram `typing` chat actions while work is queued or running. These indicators are transient Telegram UI state; they do not create messages and they stop when the bot sends a message.
