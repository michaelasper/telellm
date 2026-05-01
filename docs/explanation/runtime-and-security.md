# Runtime And Security Model

`telellm` assumes Telegram group members are trusted only within their own group sandbox.

The daemon therefore keeps the powerful control surfaces on the host and gives the group a constrained execution environment.

## Host-Owned State

The host daemon owns:

- Telegram bot credentials.
- The upstream provider credential in broker API key mode.
- The host Codex auth file source in ChatGPT OAuth mode.
- Durable SQLite memory.
- Broker token registry and rate-limit state.
- Sandbox lifecycle operations.
- Runtime status and generation tracking.
- Telegram sending.

This state is not mounted into group workspaces.

## Group-Controlled State

The group controls:

- Messages sent to its Codex session.
- Files inside its `/workspace` named volume.
- Runtime commands for its own chat.
- Group memory commands exposed by the bot.

Anyone in the group can issue group-scoped commands. The current implementation does not distinguish group administrators from ordinary members.

## Sandbox Boundary

The sandbox is a Docker container with a persistent named volume at `/workspace`.

It is created with `NET_ADMIN` because the entrypoint installs `iptables` and `ip6tables` rules. It also uses `no-new-privileges` and runs the final command as the unprivileged `codex` user.

The entrypoint allows DNS and the configured broker endpoint, then rejects loopback, private, link-local, multicast, and local IPv6 ranges.

The Docker socket is not mounted. Arbitrary host paths are not mounted except the configured Codex auth file in `chatgpt_oauth` mode.

## Credential Boundary

`telellm` supports two Codex credential modes.

In `broker_api_key` mode, the upstream provider key stays on the host. A sandbox receives a generated broker token as `OPENAI_API_KEY`. That token is scoped to one chat in the host broker registry and can be rotated.

The broker forwards only supported API paths and applies per-token request and concurrency limits. A sandbox-visible token is therefore less powerful than the host upstream credential.

In `chatgpt_oauth` mode, the host Codex `auth.json` is mounted read-only into the container at a staging path, then copied into `/home/codex/.codex/auth.json` for the unprivileged `codex` user. This enables ChatGPT/Codex subscription authentication without `OPENAI_API_KEY`, but the active sandbox can read the copied auth file. Use this mode only for groups trusted with that subscription credential inside their own sandbox.

## Lifecycle Boundaries

`/reset`, `/restart`, and `/rebuild` replace the active session generation. Stale responses from old generations are suppressed.

`/reset --clear-workspace` and `/rebuild --clear-workspace` remove the group workspace volume. Rebuild operations rotate broker tokens in `broker_api_key` mode. In `chatgpt_oauth` mode, a daemon cold start recreates existing containers so the selected auth mount and copied credentials match the current config.

## Remaining Risks

The sandbox has public internet access by design. Public dependencies, code execution, and model-generated commands can still be risky inside the group workspace.

The network policy is enforced inside the container entrypoint. Operators should keep the Docker image and ignored integration tests current, because changes in Docker, Colima, or Linux networking can affect enforcement.

Telegram access control is a daemon allow-list, not a Telegram permission system. Keep `allowed_chat_ids` explicit outside of early testing.
