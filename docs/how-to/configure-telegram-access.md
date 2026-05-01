# How To Configure Telegram Access

This guide shows how to control which Telegram groups can use a `telellm` daemon.

## Set The Bot Username

Set `telegram.bot_username` to the username BotFather assigned, without the leading `@`:

```toml
[telegram]
bot_username = "telellm_bot"
```

The username is used for mention detection and command routing.

## Restrict Allowed Chats

Set `allowed_chat_ids` to the numeric Telegram chat IDs that should be accepted:

```toml
[telegram]
allowed_chat_ids = [-1001234567890]
```

An empty list allows all chats that can reach the bot:

```toml
allowed_chat_ids = []
```

Use an empty list only while testing. Before adding the bot to broader groups, replace it with explicit chat IDs.

## Discover A Group Chat ID

Add the bot to a test group, then mention the bot or send `/status`.

If the chat is not yet in `allowed_chat_ids`, temporarily leave the list empty, trigger one message, and inspect Telegram `getUpdates` output for the numeric group chat ID:

```bash
curl "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/getUpdates"
```

Do this before starting the daemon, or while the daemon is stopped, because `telellm` also uses Telegram polling.

Look for `message.chat.id` in the JSON response. Group IDs are commonly negative and often start with `-100`.

After you have the ID, set it in `config.toml` and restart the daemon.

## Configure Privacy Mode

In BotFather, disable group privacy if the daemon should receive ambient group messages for recent context.

With privacy enabled, Telegram usually delivers commands, mentions, and replies. With privacy disabled, ordinary group messages can reach the daemon and be held in the in-memory rolling buffer.

## Rotate The Telegram Token

If the Telegram token is exposed, rotate it in BotFather, update the environment variable named by `telegram.bot_token_env`, and restart the daemon:

```bash
export TELEGRAM_BOT_TOKEN="new-token"
cargo run -- run --config config.toml
```
