# telellm Documentation

This documentation is organised by user need.

The canonical user documentation lives in `tutorials/`, `how-to/`, `reference/`, and `explanation/`. The `superpowers/` directory contains historical internal planning records from implementation work; use it for provenance, not for current setup or operations.

## Learning By Doing

Use the tutorial when you want one reliable path to a working local daemon:

- [Run telellm locally for the first time](tutorials/first-run.md)

## Working On Tasks

Use the how-to guides when you already know what you want to accomplish:

- [How to configure Telegram access](how-to/configure-telegram-access.md)
- [How to configure Codex authentication](how-to/configure-codex-auth.md)
- [How to send Telegram attachments to Codex](how-to/send-attachments.md)
- [How to use voice and audio](how-to/use-voice-and-audio.md)
- [How to send generated files back to Telegram](how-to/send-generated-files.md)
- [How to manage a group runtime](how-to/manage-group-runtime.md)
- [How to manage group memory](how-to/manage-group-memory.md)
- [How to validate and troubleshoot a setup](how-to/validate-and-troubleshoot.md)

## Looking Up Facts

Use reference pages when you need exact commands, fields, defaults, or runtime facts:

- [CLI reference](reference/cli.md)
- [Configuration reference](reference/configuration.md)
- [Telegram command reference](reference/telegram-commands.md)
- [Sandbox reference](reference/sandbox.md)
- [Broker reference](reference/broker.md)

## Understanding The System

Use explanations when you need the reasoning behind the implementation:

- [Architecture](explanation/architecture.md)
- [Message lifecycle](explanation/message-lifecycle.md)
- [Runtime and security model](explanation/runtime-and-security.md)
- [Memory model](explanation/memory-model.md)

## Historical Internal Notes

These files are retained as implementation history and may describe plans or design intent that differs from the current code:

- [Original bridge design spec](superpowers/specs/2026-04-30-telegram-codex-bridge-design.md)
- [Original implementation plan](superpowers/plans/2026-04-30-telegram-codex-bridge-implementation.md)
