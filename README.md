# telellm

`telellm` bridges Telegram group chats to sandboxed Codex CLI sessions.

## Documentation

Start with the documentation map:

- [Documentation index](docs/README.md)

The main paths are:

- [Run telellm locally for the first time](docs/tutorials/first-run.md)
- [How to validate and troubleshoot a setup](docs/how-to/validate-and-troubleshoot.md)
- [Configuration reference](docs/reference/configuration.md)
- [Telegram command reference](docs/reference/telegram-commands.md)
- [Architecture](docs/explanation/architecture.md)

## Quick Start

```bash
cp config.example.toml config.toml
export TELEGRAM_BOT_TOKEN=...
export OPENAI_API_KEY=...
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
cargo run -- doctor --config config.toml --create-network
cargo run -- run --config config.toml
```

The example config uses broker API key mode. To use a ChatGPT/Codex subscription login instead, run `codex login --device-auth` on the host, set `codex.auth_mode = "chatgpt_oauth"` and `codex.auth_host_path` to your host `auth.json`, then remove the need for `OPENAI_API_KEY`.

Use a Telegram test group while bringing the daemon up. Set `allowed_chat_ids` before adding the bot to broader groups.

## Development Checks

```bash
./scripts/check.sh
```

The script runs formatting, tests, and Clippy:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Security Model

Each Telegram group maps to a Docker/Colima sandbox and persistent workspace volume. In broker API key mode, the daemon owns Telegram credentials, durable memory, broker tokens, and the upstream provider credential. In ChatGPT OAuth mode, the sandbox receives a copied Codex auth file for subscription-backed Codex CLI use. The Docker socket is never mounted.

Read the full model in:

- [Runtime and security model](docs/explanation/runtime-and-security.md)
- [Sandbox reference](docs/reference/sandbox.md)
- [Broker reference](docs/reference/broker.md)
