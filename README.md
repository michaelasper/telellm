# telellm

`telellm` bridges Telegram group chats to sandboxed Codex CLI sessions.

## Local Setup

1. Copy `config.example.toml` to `config.toml`.
2. Set `TELEGRAM_BOT_TOKEN`.
3. Set `OPENAI_API_KEY` for the host broker.
4. Build the sandbox image:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
```

5. Run checks:

```bash
./scripts/check.sh
```

6. Start the daemon:

```bash
cargo run -- --config config.toml
```

## Security Model

Each Telegram group maps to a Docker/Colima sandbox and persistent workspace volume. The daemon owns Telegram credentials, durable memory, and long-lived provider credentials. The sandbox receives only scoped access to the host broker and must not receive arbitrary host mounts or the Docker socket.
