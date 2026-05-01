# Send Telegram Attachments To Codex

Use this when you want Codex to inspect a Telegram photo or file.

## Send An Image Or File

Send a photo or document in a DM, or send it in a group with a caption that addresses the bot:

```text
@telellm_bot what is in this image?
```

You can also reply to a bot message with a photo or document. Replies to the bot are treated as addressed messages even without a caption.

To ask about an earlier image or document in a group, reply to that message and mention the bot:

```text
@telellm_bot what is this?
```

## Where Files Go

For addressed messages, `telellm` downloads supported Telegram attachments from the triggering message and from the replied-to message, then imports them into the chat sandbox:

```text
/workspace/telegram_uploads/msg-<message-id>/<index>-<filename>
```

The prompt sent to Codex includes the imported path as an `@telegram_uploads/...` file reference. Codex can then inspect the file from the chat workspace. Image understanding depends on the configured Codex CLI and model capabilities, but the image file is available inside `/workspace`.

## Configure Limits

Attachment handling is controlled by `[attachments]`:

```toml
[attachments]
enabled = true
workspace_dir = "telegram_uploads"
max_file_bytes = 20000000
```

Files larger than `max_file_bytes` are not downloaded. The prompt still notes that the Telegram message included an attachment and explains why it was skipped.

## Supported Inputs

`telellm` currently imports Telegram photos and documents. Captions are used as the message text for addressing and prompt context.
