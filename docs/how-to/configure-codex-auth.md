# How To Configure Codex Authentication

This guide shows how to choose the credential path Codex uses inside group sandboxes.

## Use ChatGPT OAuth

Use this mode when you want Codex CLI to authenticate with a ChatGPT/Codex subscription login instead of a host OpenAI API key.

Log in on the host:

```bash
codex login --device-auth
```

Find the generated host auth file. For a default Codex install, it is usually:

```text
~/.codex/auth.json
```

Set the Codex auth mode and auth file path in `config.toml`:

```toml
[codex]
auth_mode = "chatgpt_oauth"
auth_host_path = "/Users/you/.codex/auth.json"
```

Use an absolute path. In this mode, `telellm` does not start the host broker and does not require `OPENAI_API_KEY`.

Rebuild the sandbox image after pulling this feature:

```bash
docker build -f Dockerfile.sandbox -t telellm-sandbox:local .
```

Run the doctor:

```bash
cargo run -- doctor --config config.toml --create-network
```

The report should include passing checks for the Codex auth file and `codex login status`.

## Use Broker API Key Mode

Use this mode when you want the host daemon to hold an upstream API key and expose only a scoped broker token to each sandbox.

Set the auth mode:

```toml
[codex]
auth_mode = "broker_api_key"

[broker]
listen = "127.0.0.1:8189"
public_base_url = "http://host.docker.internal:8189/v1"
upstream_base_url = "https://api.openai.com/v1"
upstream_api_key_env = "OPENAI_API_KEY"
```

Export the environment variable named by `broker.upstream_api_key_env`:

```bash
export OPENAI_API_KEY="sk-..."
```

This is the backwards-compatible default when `codex.auth_mode` is omitted.

## Switch Modes

Change `codex.auth_mode`, update the fields required by the new mode, then restart the daemon.

On the first runtime start for each chat after restart, `telellm` recreates the existing container while keeping the workspace volume. That ensures the container has the correct broker environment or OAuth auth file mount for the selected mode.
