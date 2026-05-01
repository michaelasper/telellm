# How To Validate And Troubleshoot A Setup

This guide shows how to check a local `telellm` setup and narrow down common failures.

## Run The Doctor

Run:

```bash
cargo run -- doctor --config config.toml
```

The doctor checks:

- Config loading and validation.
- The Telegram token environment variable.
- The upstream API key environment variable in `broker_api_key` mode.
- The Codex auth file and `codex login status` in `chatgpt_oauth` mode.
- Docker CLI availability.
- Sandbox image availability.
- Docker network availability.
- `codex --version` inside the configured sandbox image and network.

To create a missing configured Docker network, run:

```bash
cargo run -- doctor --config config.toml --create-network
```

## Build A Missing Sandbox Image

If the doctor reports that the sandbox image is not inspectable, build it:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
```

Run the doctor again after the build.

## Create A Missing Docker Network Manually

If you do not want the doctor to create the network, create it with Docker:

```bash
docker network create telellm_public
```

Use the network name from `docker.network` if your config uses a different value.

## Check Rust Code Health

Run:

```bash
./scripts/check.sh
```

The script runs:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Run Docker Smoke Tests

After the sandbox image exists, run ignored Docker tests explicitly:

```bash
cargo test --test docker_sandbox -- --ignored
cargo test --test codex_sandbox_smoke -- --ignored
```

These tests require Docker or Colima and the `telellm-sandbox:local` image.

## Interpret Telegram Runtime Errors

If the bot says it could not start the group's Codex runtime, check:

- Docker is running.
- The configured image exists.
- The configured network exists.
- In `broker_api_key` mode, the broker `public_base_url` host and port are reachable from containers.
- In `chatgpt_oauth` mode, `codex.auth_host_path` points to a non-empty host `auth.json` created by `codex login --device-auth`.
- The daemon logs include the runtime error detail.

If `/status` reports `degraded`, fix the reported cause and run `/restart`, `/reset`, or `/rebuild`.

## Check Codex Turn Timeouts

In `codex exec` mode, `codex_max_turn_secs` bounds the full Codex process. Interactive PTY mode also uses first-byte and inactivity limits. Relevant limits are:

- `codex_first_byte_timeout_secs`
- `codex_inactivity_secs`
- `codex_max_turn_secs`
- `codex_max_output_bytes`

Increase these limits if valid turns are timing out or being capped.

## Tune Telegram UX

Queue acknowledgements, typing indicators, response streaming, and Telegram formatting are controlled by `[telegram_ux]`.

If Telegram edit rate limits appear in logs, increase `streaming_update_interval_millis` or `streaming_min_delta_chars`. If partial updates are too long, lower `streaming_max_chars`; the final response still uses normal chunking.

If formatted responses are rejected by Telegram, keep `formatting_fallback_to_plain = true`. Use `formatting_escape = true` for arbitrary model output, and disable it only when the system prompt asks Codex to produce valid Telegram `markdown_v2` or `html`.

## Known Doctor Limits

The doctor does not currently verify Telegram API reachability, SQLite write permissions, or that the bot is present in a specific group.
