# Sandbox Reference

Each allowed Telegram group gets a Docker sandbox and a persistent workspace volume.

## Sandbox IDs

Container names are derived from the numeric Telegram chat ID:

| Chat ID | Sandbox ID |
| ---: | --- |
| `12345` | `telellm-chat-12345` |
| `-10012345` | `telellm-chat-neg10012345` |

Workspace volumes use:

```text
<workspace_volume_prefix>_<sandbox_id>
```

With the example config, chat `-10012345` uses:

```text
telellm_workspace_telellm-chat-neg10012345
```

## Container Creation

In `broker_api_key` mode, the Docker backend runs containers with:

```text
docker run -d
  --name <sandbox_id>
  --network <docker.network>
  --cap-add NET_ADMIN
  --security-opt no-new-privileges
  -v <workspace_volume>:/workspace
  -e TELELLM_BROKER_HOST=<broker host>
  -e TELELLM_BROKER_PORT=<broker port>
  -e OPENAI_BASE_URL=<broker public_base_url>
  -e OPENAI_API_KEY=<generated broker token>
  -w /workspace
  <docker.image>
  sleep infinity
```

The sandbox-visible `OPENAI_API_KEY` is a generated broker token, not the host upstream API key.

In `chatgpt_oauth` mode, broker environment variables are omitted and the host Codex auth file is mounted read-only at a staging path:

```text
docker run -d
  --name <sandbox_id>
  --network <docker.network>
  --cap-add NET_ADMIN
  --security-opt no-new-privileges
  -v <workspace_volume>:/workspace
  -v <codex.auth_host_path>:/run/telellm/codex-auth.json:ro
  -w /workspace
  <docker.image>
  sleep infinity
```

## Codex Execution

The runtime starts Codex with:

```text
docker exec -i --user codex <sandbox_id> \
  sh -lc '<wrapper>' telellm-codex-exec \
  <codex.command> <codex.args...> --model <codex.model> --cd /workspace
```

For `codex exec` mode, the wrapper passes the rendered Telegram prompt on stdin, asks Codex to write the final answer with `--output-last-message`, and returns only that final answer to Telegram. Older interactive PTY mode is still available when `codex.args` does not start with `exec`, but it is not recommended for Telegram because terminal UI repaint text can leak into replies.

## Sandbox Image

`Dockerfile.sandbox` is based on `node:24-bookworm-slim`.

It installs:

- `ca-certificates`
- `curl`
- `git`
- `iproute2`
- `iptables`
- `openssh-client`
- `procps`
- `python3`
- `python3-reportlab`
- `util-linux`
- `@openai/codex`

It creates a `codex` user and uses `/workspace` as the working directory.

## Entrypoint Network Policy

`scripts/sandbox-entrypoint.sh` runs as root, configures firewall rules, copies a staged Codex auth file into `/home/codex/.codex/auth.json` when present, writes a minimal Codex config that trusts `/workspace`, fixes `/workspace` ownership, and then executes the command as the `codex` user.

Allowed before reject rules:

- Nameservers from `/etc/resolv.conf`.
- Docker DNS, default `127.0.0.11`, configurable with `DOCKER_DNS_IP`.
- The configured broker host and port, when broker mode supplies them.

Rejected IPv4 ranges:

- `0.0.0.0/8`
- `10.0.0.0/8`
- `169.254.0.0/16`
- `172.16.0.0/12`
- `192.168.0.0/16`
- `224.0.0.0/4`

Rejected IPv6 ranges when `ip6tables` is available:

- `::/128`
- `::1/128`
- `fc00::/7`
- `fe80::/10`
- `ff00::/8`

## Lifecycle Operations

| Operation | Docker Behaviour | Workspace |
| --- | --- | --- |
| Ensure runtime with an existing in-process broker token | Inspect, start existing stopped container, or run a new one. | Kept. |
| Ensure runtime with a newly generated broker token | Remove and recreate the container so `OPENAI_API_KEY` contains the generated broker token. This includes the first runtime start for a chat after daemon process start, even when an old container exists. | Kept. |
| Ensure runtime in `chatgpt_oauth` mode after daemon cold start | Remove and recreate the container so the selected auth file mount and copied Codex credentials are present. | Kept. |
| Restart with an existing in-process broker token | `docker restart <sandbox_id>`. | Kept. |
| Restart with a newly generated broker token | Remove and recreate the container so `OPENAI_API_KEY` contains the generated broker token. | Kept. |
| Rebuild | `docker rm -f <sandbox_id>`, then run a new one. | Kept unless clear requested. |
| Clear workspace | `docker volume rm <workspace_volume>`. | Removed. |
