# Run telellm Locally For The First Time

This tutorial takes you from a fresh checkout to a running `telellm` daemon connected to a Telegram test group.

You will create a local config, build the sandbox image, let the doctor create the Docker network, and start the daemon. At the end, a message that mentions your bot in Telegram will be routed into a group-scoped Codex session.

## Before You Start

You need:

- Rust with Cargo.
- Docker or Colima with the Docker CLI available.
- A Telegram bot created in BotFather.
- Either a host OpenAI API key or a Codex CLI ChatGPT login.

## 1. Create A Local Config

Copy the example config:

```bash
cp config.example.toml config.toml
```

Open `config.toml` and set the bot username to the username BotFather gave you, without the leading `@`:

```toml
[telegram]
bot_username = "your_bot_username"
allowed_chat_ids = []
```

Leave `allowed_chat_ids` empty for this first test group. You will restrict it after the first run.

## 2. Choose Codex Authentication

The example config uses broker API key mode. Export the Telegram token and upstream provider key in the shell that will run the daemon:

```bash
export TELEGRAM_BOT_TOKEN="123456:telegram-token-from-botfather"
export OPENAI_API_KEY="sk-..."
```

For ChatGPT subscription OAuth instead, log in with Codex on the host:

```bash
codex login --device-auth
```

Then set:

```toml
[codex]
auth_mode = "chatgpt_oauth"
auth_host_path = "/Users/you/.codex/auth.json"
```

With `chatgpt_oauth`, keep `TELEGRAM_BOT_TOKEN` exported but `OPENAI_API_KEY` is not required.

## 3. Build The Sandbox Image

Build the local image used for each group sandbox:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
```

You will see Docker build the Node-based image and install the Codex CLI.

## 4. Run The Doctor

Run the doctor and allow it to create the configured Docker network:

```bash
cargo run -- doctor --config config.toml --create-network
```

You should see a `telellm doctor report`. A successful report includes passing checks for config, Telegram auth, Docker, the sandbox image, the Docker network, and `codex --version`. In broker API key mode it also checks the upstream API key environment variable. In ChatGPT OAuth mode it checks the Codex auth file and `codex login status`.

## 5. Add The Bot To A Test Group

Add the bot to a Telegram group that you control.

In BotFather, disable group privacy for the bot if you want ambient group chat to be included in recent context. With privacy enabled, Telegram will generally deliver only commands, mentions, and replies to the bot.

## 6. Start The Daemon

Start the long-running process:

```bash
cargo run -- run --config config.toml
```

The daemon connects to Telegram polling and waits for messages. In broker API key mode it also starts the host broker.

## 7. Send A First Message

In the test group, mention the bot:

```text
@your_bot_username say hello from this group
```

The first addressed message for a group may take longer because `telellm` creates the group sandbox, starts the container, launches Codex through a PTY, prepares the selected auth path, and builds the prompt context.

You have a local `telellm` daemon running with one Telegram group connected to its own sandboxed Codex session.

## Next Step

Restrict the bot to known groups before broader use:

- [How to configure Telegram access](../how-to/configure-telegram-access.md)
