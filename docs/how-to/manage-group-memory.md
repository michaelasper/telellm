# How To Manage Group Memory

This guide shows how to inspect and clear durable memory for one Telegram group.

## Store A Memory

Send:

```text
/remember Mike prefers concise status updates
```

The daemon stores the text as a durable host-managed `personality` memory for the current group.

The sandbox can see remembered facts as prompt context, but it cannot edit the host SQLite memory store directly.

## List Durable Memory

Send:

```text
/memory
```

The bot replies with the durable records currently stored for the group.

## Clear All Durable Memory

Send:

```text
/forget all
```

The daemon deletes all durable memory records for the current group and replies with the number of removed records.

## Request Targeted Forgetting

Send:

```text
/forget Mike prefers concise status updates
```

The current implementation acknowledges targeted forget requests but does not delete matching records. Use `/forget all` when you need memory removed today.

## Preserve Working Files

Memory commands affect only the host-managed memory database. They do not remove files in the group's `/workspace` volume.

Use runtime commands with `--clear-workspace` when the workspace itself should be removed:

```text
/reset --clear-workspace
/rebuild --clear-workspace
```
