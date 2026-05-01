# Architecture

`telellm` is a local Rust daemon that bridges Telegram group chats to Codex CLI sessions running inside Docker or Colima containers.

The design separates group-controlled execution from host-owned control surfaces. Telegram group members can influence their own sandbox and workspace, but they should not gain access to host credentials, host files, durable memory storage, or Docker control.

## Main Components

The binary starts from `src/main.rs`. It parses the CLI, loads config, and calls `app::run`.

`app::run` wires together:

- A Telegram polling adapter.
- A Telegram sending sink with message chunking.
- A SQLite-backed memory store.
- An in-memory rolling message buffer.
- A per-group router.
- A runtime manager.
- An Axum broker for sandbox API traffic when `codex.auth_mode = "broker_api_key"`.

Most modules own one boundary:

| Module | Responsibility |
| --- | --- |
| `bot` | Telegram normalisation, addressing, command parsing, and sending. |
| `router` | Per-group bounded queues and serialised session work. |
| `runtime` | Per-chat sandbox lifecycle, auth-mode setup, and Codex session creation. |
| `sandbox` | Docker command construction and network policy data. |
| `codex` | PTY-backed session supervision and output cleanup. |
| `memory` | Durable memories, recent message buffer, and prompt context rendering. |
| `broker` | Scoped sandbox authentication, rate limits, and upstream proxying. |
| `config` | TOML loading, defaults, and validation. |

## Per-Group Runtime Shape

Each Telegram group maps to:

- One Docker container name derived from the chat ID.
- One Docker named volume mounted at `/workspace`.
- One selected Codex credential path: a generated broker token or a copied ChatGPT OAuth auth file.
- One Codex execution strategy for the active generation. The recommended strategy is non-interactive `codex exec` per queued turn inside the persistent sandbox.
- One router worker queue for the active session generation.

The runtime manager creates these lazily. The first addressed message or runtime command that needs Codex starts the group runtime.

## Data Flow

An addressed Telegram message flows through the system like this:

1. Teloxide receives a Telegram update.
2. The bot adapter normalises it into `IncomingMessage`.
3. `AppCore` stores it in the rolling buffer.
4. Command parsing runs first.
5. Non-command ambient messages stop there.
6. Addressed messages ensure a group runtime exists.
7. The app fetches recent messages and durable memories.
8. `ContextPacket` renders the prompt.
9. The router enqueues the prompt for the group.
10. The group worker sends the prompt to Codex inside the sandbox.
11. The Codex final message is sent back to Telegram in chunks.

The system does not stream tokens to Telegram. In `codex exec` mode, it sends a completed turn when the Codex process exits and writes its final message.

## Why A Broker Exists

The sandbox is intentionally group-controlled. Passing long-lived provider credentials directly into that environment would give group members too much authority.

Instead, the sandbox receives:

- `OPENAI_BASE_URL` pointing at the host broker.
- `OPENAI_API_KEY` containing a generated, scoped broker token.

The host broker validates the token, applies per-token limits, and forwards to the upstream provider with the host-held credential.

This keeps the provider credential outside the group workspace while still allowing Codex to call an OpenAI-compatible API.

## Why ChatGPT OAuth Mode Exists

Some deployments need Codex CLI to use a ChatGPT/Codex subscription login instead of an API key. In `chatgpt_oauth` mode, the runtime skips the broker, mounts the host Codex `auth.json` into the container at a staging path, and the entrypoint copies it into the `codex` user's home.

That mode uses the Codex CLI's own authentication path. It is operationally simpler for subscription-backed usage, but the copied auth file is available inside the active group sandbox.
