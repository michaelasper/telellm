# Send Generated Files Back To Telegram

Use this when you want Codex to create a file, such as a PDF, CSV, Markdown file, or small archive, and have `telellm` upload it to Telegram.

## Ask For A File

Ask the bot to create the artifact and send it:

```text
@telellm_bot create a one-page PDF summary of the deployment plan
```

For each Codex turn, `telellm` adds an instruction telling Codex to write sendable files under:

```text
/workspace/telegram_outputs/
```

Codex must mention each file in the final answer as an `@...` reference:

```text
@telegram_outputs/deployment-plan.pdf
```

When the final answer contains a valid reference, `telellm` copies that file out of the chat sandbox and sends it as a Telegram document.

## Configure Output Files

Generated-file handling is controlled by `[outputs]`:

```toml
[outputs]
enabled = true
workspace_dir = "telegram_outputs"
max_file_bytes = 20000000
max_files_per_response = 4
```

`workspace_dir` must be relative to `/workspace`. `telellm` only exports files that are mentioned in the final answer and live under that directory.

## PDF Generation

The sandbox image includes Python and ReportLab, so Codex can create PDFs by writing a small Python script inside `/workspace`.

After changing `Dockerfile.sandbox`, rebuild the image before expecting PDF tooling in existing deployments:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
```

Then restart or rebuild the group runtime so the chat sandbox uses the updated image.

## Limits

Files larger than `outputs.max_file_bytes` are not sent. A response can send up to `outputs.max_files_per_response` files.

If Codex mentions a missing, oversized, or invalid file, `telellm` keeps the text response and appends a short note that the file could not be attached.
