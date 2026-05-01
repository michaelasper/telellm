# Telegram Codex Bridge Design

## Summary

Build a Rust daemon that puts a Codex-powered assistant into Telegram group chats. The assistant responds only when addressed, but it observes group conversation to maintain recent context and long-term personality memory. Each Telegram group receives its own persistent Docker/Colima sandbox with a long-lived Codex CLI session. The group has broad control inside that sandbox, while the host machine remains outside the group trust boundary.

## Goals

- Run on the local Mac as an always-on service.
- Connect Telegram group chats to Codex CLI.
- Maintain one persistent sandbox and one long-lived Codex session per group.
- Let anyone in a group control that group's sandbox and memory through group-scoped commands.
- Preserve social continuity with a short raw catch-up buffer and curated long-term memory.
- Allow full external internet from sandboxes while blocking access to the host and local networks.
- Keep Telegram credentials, long-lived Codex/provider credentials, host memory, and daemon configuration outside the group-controlled sandbox.
- Use idiomatic Rust with Tokio, typed errors, clear module boundaries, tests, `rustfmt`, and `clippy`.

## Non-Goals

- Building a collaborative coding environment.
- Storing full raw Telegram history indefinitely.
- Letting sandboxes access host files, host services, Docker internals, or private LAN addresses.
- Creating a web UI in the first version.
- Supporting multiple sandbox backends in the first implementation, beyond designing the boundary so another backend can be added later.

## Core Architecture

The application is a Rust daemon with three major responsibilities:

1. Listen to Telegram group messages and commands.
2. Manage group-specific runtime state, memory, and routing.
3. Supervise a Docker/Colima sandbox and Codex CLI PTY per group.

The host daemon owns all sensitive and durable host-side state:

- Telegram bot token.
- Codex credential source.
- Group registry.
- Long-term memory and personality profiles.
- Rolling message buffers.
- Sandbox lifecycle controls.
- Network policy configuration.
- Logs, metrics, and health state.

The group controls only its own sandbox scope:

- Messages sent into the group Codex session.
- Files and tools inside the group workspace volume.
- Group-scoped reset, restart, rebuild, memory, forget, status, and help commands.

Normal flow:

1. A Telegram update arrives.
2. The bot normalizes the update into an internal event.
3. The router determines whether it is an ambient message, command, mention, or reply to the bot.
4. Ambient messages update the recent buffer and may later feed memory summarization.
5. Addressed messages and commands are serialized through the group's work queue.
6. The daemon builds a context packet from the triggering message, recent buffer, relevant long-term memory, participant metadata, and group personality.
7. The Codex PTY supervisor writes the packet into the group's long-lived Codex session.
8. Output streams back to Telegram with chunking, status messages, and rate-limit handling.

## Chat Behavior

The bot responds only when explicitly addressed:

- Mentioning the bot by username.
- Replying to a bot message.
- Sending supported slash commands.

The bot still observes ambient group messages. Ambient observation does not produce replies. Its purpose is to keep the assistant caught up when mentioned later and to allow personality memory to grow over time.

The first command set should include:

- `/help`: show group-scoped commands.
- `/status`: show sandbox, Codex session, queue, and memory status.
- `/reset`: reset the Codex session and optionally clear the workspace.
- `/restart`: restart the Codex process inside the existing sandbox.
- `/rebuild`: recreate the group container from the pinned image.
- `/memory`: show or summarize current durable memory for the group.
- `/forget`: delete group memory entries or clear durable group memory.

Anyone in the Telegram group can run these commands for that group. Host-level controls and secrets remain unavailable from chat.

## Memory Model

Memory is hybrid:

- Host-managed memory stores durable identity, preference, relationship, and personality facts.
- Sandbox-local files store working notes and artifacts that the group can inspect or modify.

The daemon stores a short raw rolling buffer per group for catch-up. Raw buffered messages expire. They are not permanent transcript storage.

Durable memory stores distilled facts such as:

- User names, aliases, and preferences.
- Recurring topics.
- Relationship context between participants.
- Group norms.
- Running jokes and shared references.
- Personality notes for how the assistant should behave in that group.

Memory writes happen through a host-managed summarization path. Codex can receive memory as context, but it cannot directly edit the durable host memory store through shell access. If summarization fails, normal chat handling continues and the daemon logs the failure.

## Sandbox And Network Policy

Each Telegram group maps to:

- One Docker/Colima container.
- One persistent workspace volume.
- One long-lived Codex CLI process supervised through a PTY.

The sandbox has full external internet access but must not reach the host or local networks. The policy must block:

- Host loopback and host gateway routes.
- Docker and Colima bridge addresses.
- RFC1918 private ranges.
- Link-local addresses.
- Local-only service discovery paths.
- Direct access to the Docker socket or host filesystem.

On macOS with Colima, Docker network flags alone may not express this policy completely. The implementation should treat network isolation as a required feature and verify it with integration tests. If needed, the sandbox image can route traffic through a restrictive proxy or firewall entrypoint that denies local CIDRs and permits public internet destinations.

Long-lived Codex/provider credentials must not be written to the workspace volume or placed broadly in the group-controlled container environment. A powerful sandbox can often inspect files, inherited environments, process metadata, or command output, so read-only credentials are not enough by themselves.

The v1 credential model is a host-managed auth broker. The broker exposes only the narrow API surface needed by Codex, applies group-scoped rate limits, forwards to the real provider with host-held credentials, and never returns raw provider credentials to the sandbox.

The broker is an explicit exception to the general "no local network" rule: the sandbox may reach only that broker endpoint, not arbitrary host or LAN services. The broker must not expose host files, shell execution, Docker control, raw provider credentials, or unrelated local services.

If implementation proves that Codex CLI cannot use a broker-compatible endpoint, the fallback is a revocable, group-scoped, rate-limited token that is safe to treat as sandbox-visible and can be rotated on reset or compromise. Long-lived provider credentials still must not enter the sandbox.

## Rust Code Structure

The codebase should start as one Rust package with focused modules. It can later split into workspace crates if module boundaries become large enough to justify it.

- `app`: application wiring, startup, shutdown, tracing, and health.
- `bot`: Telegram adapter, update normalization, addressing rules, command parsing, reply chunking, and rate-limit handling.
- `router`: chat-to-runtime mapping, per-group queues, request serialization, and backpressure.
- `sandbox`: sandbox backend traits and the Docker/Colima implementation.
- `codex`: PTY supervision, prompt writing, output streaming, restart handling, and inactivity timeouts.
- `memory`: rolling buffers, durable memory, personality profiles, and memory selection.
- `config`: typed configuration, validation, secret source definitions, and runtime limits.

The design should use:

- Tokio for async orchestration.
- Bounded `mpsc` channels for per-group work queues.
- `watch` or `broadcast` channels for shutdown and status changes.
- Small typed IDs for `ChatId`, `UserId`, `MessageId`, and `SandboxId`.
- `thiserror` for module and domain errors.
- `anyhow` only at the binary boundary if it helps startup error reporting.
- Borrowing in public APIs where ownership is not required.
- `Arc` for shared services that must cross async task boundaries.
- No `unsafe` code in the initial implementation.

Traits should be introduced at meaningful boundaries, not everywhere by default:

- `SandboxBackend` for container lifecycle operations.
- `CodexSession` or `CodexSupervisor` for PTY-backed session control.
- `MemoryStore` for durable memory persistence.
- `TelegramSink` for reply output in tests.

Use generics/static dispatch where concrete types are known. Use `Arc<dyn Trait + Send + Sync>` only at runtime composition boundaries where swappable implementations are useful.

## Failure Handling

Group-visible errors should be explicit and safe:

- If Codex is busy, report queued or busy status.
- If the Codex PTY exits, mark the session degraded and allow `/restart`.
- If Docker/Colima is unavailable, report that the sandbox backend is down.
- If sandbox startup fails, keep the group registered but degraded.
- If memory summarization fails, continue chat handling and log the error.
- If Telegram rate limits responses, queue or truncate follow-up chunks with a clear status.

Timeouts should be layered:

- Telegram send timeout.
- Codex inactivity timeout.
- Maximum active run duration.
- Sandbox start/stop/rebuild timeout.
- Memory summarization timeout.

The router must serialize writes to a group's Codex PTY so concurrent group messages do not corrupt the live session.

## Data Storage

Initial storage should be local and simple:

- A config file for daemon settings.
- A local database for group registry, memory, personality, rolling buffers, and session metadata.
- Docker named volumes for group workspaces.

SQLite is the preferred first durable store because it is local, reliable, and easy to back up. The implementation should keep persistence behind a `MemoryStore` and group registry abstraction so the storage can change later without affecting bot or router logic.

## Security Invariants

- Telegram group members are trusted only inside their own group sandbox.
- Group members can reset or clear their own group memory, but cannot access host secrets.
- Sandboxes must not receive long-lived provider credentials; any sandbox-visible auth material must be scoped, revocable, and rate-limited.
- Codex receives host-managed memory as prompt context, not as writable host files.
- The daemon never bind-mounts arbitrary host paths into group containers.
- The Docker socket is never mounted into a group container.
- The sandbox network policy must deny host and local network addresses while allowing external internet.
- Logs should avoid printing Telegram tokens, Codex credentials, and raw secrets.

## Testing Strategy

Unit tests should cover pure logic first:

- Command parsing.
- Bot addressing rules.
- Telegram message chunking.
- Memory retention and promotion decisions.
- Config validation.
- Router serialization and backpressure behavior.
- Docker command and network policy construction.

Integration tests should cover:

- Creating a disposable sandbox.
- Starting and stopping a PTY-backed process.
- Sending a prompt-like input and streaming output.
- Verifying blocked local network targets from inside the sandbox.
- Verifying allowed public internet targets from inside the sandbox.

Rust validation should include:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo test --doc` once public library APIs have doctests

Test names should describe behavior, and tests should generally validate one behavior at a time.

## Implementation Sequence

1. Scaffold the Rust package and configuration model.
2. Build Telegram update normalization, command parsing, and addressing rules.
3. Add local persistence for group registry, rolling buffers, and memory records.
4. Implement the router with per-group bounded queues.
5. Implement Docker/Colima sandbox lifecycle.
6. Add PTY supervision for a long-lived process in a group sandbox.
7. Wire Codex CLI into the sandbox image and session supervisor.
8. Add memory context packet construction.
9. Add sandbox network restrictions and tests.
10. Add group commands for status, reset, restart, rebuild, memory, forget, and help.
11. Add integration tests and operational docs.

## Open Decisions Resolved In This Spec

- The host stack is Rust.
- The runtime model is one persistent Docker/Colima sandbox per group.
- The Codex model is one long-lived session per group.
- The bot responds only when addressed.
- The bot observes ambient group messages for catch-up and memory.
- Ambient raw history is short-lived; durable memory is summarized.
- Anyone in the group can control group-scoped actions.
- Sandboxes get public internet but no host or local network access.
- Long-lived Codex/provider credentials stay host-managed; any sandbox-visible auth material is scoped, revocable, and not an editable workspace file.
