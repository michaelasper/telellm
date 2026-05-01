# CLI Reference

`telellm` provides `run` and `doctor` subcommands.

## Default Invocation

```bash
telellm
```

Equivalent to:

```bash
telellm run --config config.toml
```

## Legacy Config Flag

```bash
telellm --config path/to/config.toml
```

Equivalent to:

```bash
telellm run --config path/to/config.toml
```

## `run`

```bash
telellm run --config config.toml
```

Runs the Telegram polling daemon, SQLite memory store, router, and runtime manager. It also runs the host broker when `codex.auth_mode = "broker_api_key"`.

Arguments:

| Argument | Default | Description |
| --- | --- | --- |
| `--config <path>` | `config.toml` | TOML config path. |

## `doctor`

```bash
telellm doctor --config config.toml
```

Runs setup checks and exits with status `1` if any check fails.

The auth checks are mode-aware. `broker_api_key` mode checks the configured upstream API key environment variable. `chatgpt_oauth` mode checks the configured Codex auth file and probes `codex login status` in the sandbox image.

Arguments:

| Argument | Default | Description |
| --- | --- | --- |
| `--config <path>` | `config.toml` | TOML config path. |
| `--create-network` | `false` | Create the configured Docker network when it is missing. |

Report statuses:

| Status | Meaning |
| --- | --- |
| `pass` | The check succeeded. |
| `fail` | The check ran and failed. |
| `skip` | The check could not run because an earlier dependency failed. |
