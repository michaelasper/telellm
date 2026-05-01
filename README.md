# telellm

`telellm` bridges Telegram group chats to sandboxed Codex CLI sessions.

## Local Setup

1. Copy `config.example.toml` to `config.toml`.
2. Set `TELEGRAM_BOT_TOKEN`.
3. Set `OPENAI_API_KEY` for the host broker.
4. Create the bot in BotFather and add it to a test group.

If you want ambient catch-up messages to reach the daemon, disable Telegram group privacy for the bot in BotFather. Set `allowed_chat_ids` in `config.toml` before adding the bot to broader groups; use Telegram `getUpdates` or daemon logs to discover the numeric group chat ID.

5. Create the Docker network referenced by `config.example.toml`:

```bash
docker network create telellm_public
```

The `telellm_public` network must exist before the daemon starts because the example config attaches each sandbox to it. The doctor can create this network for you with `--create-network`.

6. Build the sandbox image:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
```

7. Run the setup doctor:

```bash
cargo run -- doctor --config config.toml
cargo run -- doctor --config config.toml --create-network
```

8. Run checks:

```bash
./scripts/check.sh
```

9. Start the daemon:

```bash
cargo run -- run --config config.toml
```

## Group Commands

`/remember <fact>` stores a host-managed group memory record. The sandbox can see remembered facts as prompt context, but cannot directly edit the host memory database.

## Security Model

Each Telegram group maps to a Docker/Colima sandbox and persistent workspace volume. The daemon owns Telegram credentials, durable memory, and long-lived provider credentials. The sandbox receives only scoped access to the host broker and must not receive arbitrary host mounts or the Docker socket.
