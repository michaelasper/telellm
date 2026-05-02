# Send Telegram Attachments To Codex

Use this when you want Codex to inspect a Telegram photo or file.

For Telegram voice and audio transcription, use [voice and audio](use-voice-and-audio.md).

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

## Send URLs

When URL ingestion is enabled, include `http` or `https` URLs in an addressed message:

```text
@telellm_bot compare these two posts https://example.com/a https://example.com/b
```

`telellm` fetches up to `url_ingestion.max_urls_per_message` URLs, writes readable snapshots under:

```text
/workspace/web_pages/msg-<message-id>/<index>-<host>.md
```

The prompt sent to Codex includes context notes that point at the saved `@web_pages/...` snapshots. If a URL is too large, has an unsupported content type, fails to fetch, redirects to a blocked target, or violates the SSRF policy, the prompt includes a skipped URL note and the normal text request continues.

## Supported Inputs

`telellm` imports Telegram photos and documents through `[attachments]`. Captions are used as the message text for addressing and prompt context.

Voice/audio attachments and addressed URLs use separate `[audio]` and `[url_ingestion]` limits.
