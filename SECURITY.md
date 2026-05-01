# Security Policy

## Supported Versions

`main` is the supported development line.

## Reporting A Vulnerability

Please avoid posting active secrets or exploit details in public issues. Use GitHub security advisories for private reports when available, or contact the maintainer through GitHub with a minimal description and reproduction steps.

## Operator Notes

`telellm` runs model-controlled code inside per-chat Docker sandboxes. Keep these boundaries in mind before exposing a daemon to a group:

- Set `telegram.allowed_chat_ids` to explicit group or DM IDs. An empty list allows every chat that reaches the bot.
- Anyone in an allowed chat can use that chat's runtime commands, including workspace-clearing commands.
- In `broker_api_key` mode, the upstream API key stays on the host and sandboxes receive per-chat broker tokens.
- In `chatgpt_oauth` mode, the sandbox receives a copied Codex auth file. Use this only for chats trusted with that subscription credential inside their own sandbox.
- Do not commit `profiles/`, `.env`, SQLite data, screen logs, pid files, or other local runtime files.
- Do not build Docker images from a context that includes live secrets. The repository `.dockerignore` excludes the local runtime paths used by the development profiles.

If a Telegram token, OpenAI API key, GitHub token, or Codex auth file is exposed, rotate or revoke it before continuing.
