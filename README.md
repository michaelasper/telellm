# telellm

`telellm` bridges Telegram group chats to sandboxed Codex CLI sessions.

## Local Setup

1. Copy `config.example.toml` to `config.toml`.
2. Set `TELEGRAM_BOT_TOKEN`.
3. Set `OPENAI_API_KEY` for the host broker.
4. Create the Docker network referenced by `config.example.toml`:

```bash
docker network create telellm_public
```

The `telellm_public` network must exist before the daemon starts because the example config attaches each sandbox to it. The doctor can create this network for you with `--create-network`.

5. Build the sandbox image:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
```

6. Run the setup doctor:

```bash
cargo run -- doctor --config config.toml
cargo run -- doctor --config config.toml --create-network
```

7. Run checks:

```bash
./scripts/check.sh
```

8. Start the daemon:

```bash
cargo run -- run --config config.toml
```

## Security Model

Each Telegram group maps to a Docker/Colima sandbox and persistent workspace volume. The daemon owns Telegram credentials, durable memory, and long-lived provider credentials. The sandbox receives only scoped access to the host broker and must not receive arbitrary host mounts or the Docker socket.
