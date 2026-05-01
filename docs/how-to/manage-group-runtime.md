# How To Manage A Group Runtime

This guide shows how to inspect and replace the sandboxed Codex runtime for one Telegram group.

## Check Runtime Status

Send:

```text
/status
```

The bot reports one of:

- `not started`: no Codex runtime has been created for the group.
- `starting`: a lifecycle operation is in progress.
- `ready`: the group has a registered Codex session.
- `degraded`: startup or replacement failed. The detailed reason is included in `/status` and the daemon logs.

The generation number increments when a new session is registered.

## Reset The Codex Session

Send:

```text
/reset
```

This replaces the group's active session. The workspace volume is kept.

If in-flight work belongs to the old generation, its response is suppressed after replacement starts.

## Reset And Clear The Workspace

Send:

```text
/reset --clear-workspace
```

This removes the group workspace volume, recreates the sandbox, and registers a fresh session. In `broker_api_key` mode it also rotates and unregisters the broker token.

## Restart The Group Sandbox

Send:

```text
/restart
```

This restarts the Docker container for the group and registers a fresh Codex session. The workspace volume is kept.

## Rebuild The Group Sandbox

Send:

```text
/rebuild
```

This removes and recreates the group container from the configured image and keeps the workspace volume. In `broker_api_key` mode it also rotates the broker token.

To clear the workspace while rebuilding, send:

```text
/rebuild --clear-workspace
```

## Handle A Busy Group

If another request is already running for the same group, `telellm` queues the new request and replies:

```text
Queued behind N existing request(s).
```

Work is serialised per group so concurrent messages cannot overlap inside the same Codex runtime.
