# Broker Reference

The host broker gives sandboxes an OpenAI-compatible endpoint without exposing the host upstream API key.

The broker runs only when `codex.auth_mode = "broker_api_key"`. In `chatgpt_oauth` mode, sandboxes use the mounted Codex auth file directly and the host broker is not started.

## Routes

| Route | Method | Upstream Path |
| --- | --- | --- |
| `/v1/responses` | `POST` | `<upstream_base_url>/responses` |
| `/v1/chat/completions` | `POST` | `<upstream_base_url>/chat/completions` |

The broker forwards JSON request bodies to the upstream provider and returns the upstream status and body when forwarding succeeds.

## Authentication

Sandbox requests use:

```http
Authorization: Bearer <broker-token>
```

Broker tokens are generated per chat runtime and registered in the host token registry.

The optional chat metadata header is:

```http
x-telellm-chat-id: <chat-id>
```

When present, the header must match the chat that owns the token.

## Upstream Credential

The broker reads the host upstream API key from the environment variable named by:

```toml
[broker]
upstream_api_key_env = "OPENAI_API_KEY"
```

The broker sends that credential upstream as a bearer token. It does not return the host credential to the sandbox.

## Rate Limits

Rate limits are per broker token.

| Config Field | Default | Effect |
| --- | ---: | --- |
| `broker_max_requests_per_window` | `120` | Maximum accepted requests in one window. |
| `broker_rate_limit_window_secs` | `60` | Window duration. |
| `broker_max_concurrent_requests` | `4` | Maximum in-flight requests. |

Limit failures return:

```http
429 Too Many Requests
```

## Error Responses

| Status | Cause |
| --- | --- |
| `401` | Missing, malformed, unknown, or stale broker token. |
| `403 Forbidden` | Valid token used with a different `x-telellm-chat-id`. |
| `429 Too Many Requests` | Per-token rate or concurrency limit exceeded. |
| `502 Bad Gateway` | Upstream request failed or upstream response body could not be read. |

## Token Rotation

Broker tokens rotate when runtime commands clear or rebuild trust boundaries:

- `/reset --clear-workspace`
- `/rebuild`
- `/rebuild --clear-workspace`

Old tokens are unregistered, and their rate-limit state is removed.
