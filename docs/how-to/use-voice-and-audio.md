# Use Voice And Audio

Use this when you want Telegram voice or audio messages transcribed into Codex context, or when you want voice-triggered turns to receive spoken replies.

## Configure Local Tools

`telellm` runs STT and TTS commands on the host. Configure the commands in `config.toml`:

```toml
[audio]
enabled = true
replies_enabled = true
workspace_dir = "telegram_audio"
max_file_bytes = 20000000

[audio.stt]
command = "whisper-cli"
args = ["--model", "/models/ggml-base.en.bin", "--file", "{input}", "--output-txt", "{output}"]
timeout_secs = 120

[audio.tts]
command = "piper"
args = ["--model", "/models/en_US-lessac-medium.onnx", "--output_file", "{output}"]
stdin_text = true
timeout_secs = 120
send_as = "audio"
```

Check the local commands before starting the daemon:

```bash
whisper-cli --help
piper --help
```

Example STT invocation:

```bash
whisper-cli --model /models/ggml-base.en.bin --file sample.ogg --output-txt /tmp/transcript.txt
```

Example TTS invocation:

```bash
printf 'hello from telellm' | piper --model /models/en_US-lessac-medium.onnx --output_file /tmp/reply.wav
```

Match the configured arguments to the installed tool versions. `{input}` is replaced only for STT. `{output}` is replaced for STT and TTS. Plain `piper` output is a regular audio file, so the example uses `send_as = "audio"`. Use `send_as = "voice"` only with a wrapper command that produces Telegram voice-compatible audio.

## Check Runtime Availability

In Telegram, run:

```text
/voice status
```

The reply is the authoritative runtime availability check for this milestone:

```text
Audio: enabled. Spoken replies: enabled. Chat voice mode: on. STT: available. TTS: available.
```

This command is handled on the host and does not start Codex.

## Transcribe A Voice Or Audio Message

Send a Telegram voice or audio message in a DM, reply to the bot with it, or send it in a group with a caption that addresses the bot:

```text
@telellm_bot answer this
```

`telellm` downloads the audio, imports it into the chat workspace, runs STT, and adds a prompt context note like:

```text
Audio transcript from @telegram_audio/msg-42/1-voice.ogg: ...
```

If transcription fails but the message has other usable text context, the prompt includes a skipped transcript note and the normal text response continues. If a voice-only message cannot be transcribed, the bot returns a concise transcription error instead of starting Codex.

## Control Spoken Replies

Use:

```text
/voice off
/voice on
```

`/voice off` suppresses spoken replies for that chat. `/voice on` re-enables them unless `[audio].replies_enabled` is disabled by the daemon operator.

Spoken replies are only attempted for Codex turns triggered by voice or audio attachments. The bot sends the final text response first. It then sends audio only when global audio is enabled, spoken replies are globally enabled, the chat voice mode is on, and TTS is available.

TTS failures are logged and do not break the text response.
