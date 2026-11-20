# Rust Telegram Video Converter Bot

A standalone Telegram bot written in Rust. It receives a video, downloads it to a per-job temporary directory, probes it with FFprobe, tries MP4 remuxing when the streams are compatible, otherwise transcodes to H.264/AAC, validates the MP4, sends it back as a Telegram **document**, then removes the job's source and output files.

## Important limitations

- **100% quality is only possible for a compatible remux.** Remux copies encoded streams without re-encoding. A transcode changes the encoded video/audio and may slightly reduce quality; `CRF 18` is a high-quality setting, not a promise of mathematically identical quality.
- All audio streams are mapped. Text subtitles supported by MP4 (`SubRip`, `ASS/SSA`, WebVTT, `mov_text`) are converted to `mov_text`. Bitmap subtitles such as PGS/VobSub are not included because MP4 `mov_text` cannot represent them. Container metadata and chapters are copied where FFmpeg supports them.
- The first video stream is used. Additional video-angle streams, attachments, and arbitrary data streams are not carried into the MP4.
- Telegram's **hosted Bot API** has file download/upload size limits (commonly 20 MB download and 50 MB upload). For larger files, install a **Local Bot API Server** and set `BOT_API_BASE` to its HTTP endpoint. The local server may return an absolute file-system path from `getFile`; this bot can copy it only when that path is visible inside the bot process. If running in separate containers, mount the Local Bot API file directory read-only into the bot container at the same path. Local Bot API Server has its own documented limits (including a 2 GB upload maximum); this is not unlimited. Confirm current Telegram limits before deployment.
- A 512 MB RAM / 50% CPU container can still be too small for very large or complex inputs. Free disk space, processing time, and the hosting provider's limits apply.
- The bot processes one job at a time. Files are streamed to/from disk; upload uses a streaming multipart body, not a whole-file RAM buffer.

## Same-server Local Bot API setup

1. Obtain `api_id` and `api_hash` from https://my.telegram.org/apps. Keep both secret.
2. Obtain/build the official Telegram `telegram-bot-api` server executable for the server's Linux architecture and upload it beside `rust-video-converter-bot` as `telegram-bot-api`.
3. Make it executable (`chmod +x telegram-bot-api`).
4. Set `LOCAL_BOT_API_ENABLED=true`, `API_ID`, and `API_HASH` in the hosting Environment settings. Keep `BOT_TOKEN` as a separate variable.
5. Keep the startup command loading `.env` if you store settings in a file; for Pterodactyl environment variables, no `.env` load is needed. Start the Rust bot. It launches the Local Bot API Server itself and points its API requests to `http://127.0.0.1:8081`.
6. Do not set `BOT_API_BASE` to the official API while local mode is enabled; local mode overrides it.

**Resource warning:** Running both services in a 512 MB RAM / 1 GB disk container may fail or be terminated by the host, especially during large transfers/conversion. The bot cannot remove hosting limits. Leave enough disk for both the Local Bot API download and the converter's working copy/output. If the Local Bot API returns a file path that is not accessible to the bot process, both processes must share the same filesystem path.

## Requirements

- Rust stable (only if building manually)
- FFmpeg with `libx264` and AAC encoder support, plus FFprobe
- Telegram bot token from `@BotFather`
- For large files: a Local Bot API Server reachable by the bot

## Quick start with Docker

1. Create a Telegram bot with `@BotFather` and copy its token.
2. Copy `.env.example` to `.env` and set `BOT_TOKEN`.
3. For the hosted Telegram API, leave `BOT_API_BASE=https://api.telegram.org` and set `MAX_UPLOAD_MB=45`. Note: hosted Bot API download limit is lower, so large incoming files will not work.
4. For larger files, you can run the Local Bot API Server in the same container as a child process. Upload the official `telegram-bot-api` executable beside the Rust bot binary, set `LOCAL_BOT_API_ENABLED=true`, `API_ID`, and `API_HASH`. The bot starts it on `127.0.0.1:LOCAL_BOT_API_PORT` and automatically uses that endpoint. Set `MAX_UPLOAD_MB` to a value no greater than the Local Bot API Server's supported limit. This requires the Local Bot API executable for the same Linux architecture; the Rust GitHub Actions artifact does not include it.
5. Start: `docker compose up -d --build`
6. Send `/start` to your bot, then send a video as a Telegram document or video.

The compose file sets a 512 MB memory limit and 0.50 CPU quota. These limits are enforced by Docker, not just by FFmpeg's thread count. Make sure the host has enough disk space; the `data/` directory stores active jobs temporarily.

## Build manually

```bash
sudo apt-get update
sudo apt-get install -y ffmpeg pkg-config build-essential
cp .env.example .env
# Edit .env and set BOT_TOKEN
cargo build --release
./target/release/rust-video-converter-bot
```

## GitHub Actions binary

Push this project to a GitHub repository. The workflow at `.github/workflows/build.yml` builds an x86_64 Linux binary on pushes to `main`/`master` and on version tags. Download the workflow artifact from the Actions run. The binary still requires FFmpeg/FFprobe on the target server. For a tagged release, it also attaches a tarball to the GitHub Release.

## Environment variables

| Variable | Default | Meaning |
|---|---|---|
| `BOT_TOKEN` | required | Telegram bot token |
| `BOT_API_BASE` | `https://api.telegram.org` | Telegram Bot API base URL; point to Local Bot API Server for large files |
| `ALLOWED_USER_IDS` | empty | Optional comma-separated numeric Telegram user IDs; empty allows all users |
| `WORK_DIR` | `./data` | Temporary job storage |
| `CONVERSION_TIMEOUT_SECONDS` | `7200` | FFmpeg job timeout |
| `MIN_FREE_DISK_MB` | `512` | Minimum free disk space required before processing |
| `MAX_UPLOAD_MB` | `45` | Application size guard; does not override Telegram's own limits |
| `RUST_LOG` | `info` | Log verbosity |
| `LOCAL_BOT_API_ENABLED` | `false` | Start Local Bot API Server as a child process |
| `API_ID` | empty | Telegram API ID from my.telegram.org, required when local server enabled |
| `API_HASH` | empty | Telegram API hash from my.telegram.org, required when local server enabled |
| `LOCAL_BOT_API_BIN` | `./telegram-bot-api` | Path to Local Bot API Server executable |
| `LOCAL_BOT_API_PORT` | `8081` | Local HTTP API port, bound to loopback only |
| `LOCAL_BOT_API_DIR` | `./data/local-bot-api` | Local Bot API file storage directory |

## Safety and cleanup behavior

- Each job has a separate directory named from chat/message IDs.
- FFmpeg is invoked with argument arrays, not a shell command string.
- A previous-run recovery pass removes abandoned job directories at startup.
- The output is deleted only after Telegram confirms `sendDocument` success. If upload fails, the job reports failure and the temporary job directory is then cleaned up.
- Do not expose `.env` or the bot token. Rotate the token with BotFather if it leaks.

## Known production hardening still recommended

Before exposing this bot to the public, use `ALLOWED_USER_IDS` or implement a rate/abuse policy, configure disk quotas and monitoring, and test with representative media files. The initial version does not implement a persistent job database, progress percentage, per-user quotas, or cancellation commands. The polling loop is sequential and a process restart loses pending Telegram updates older than Telegram's update retention window.


## Build the Local Bot API executable with GitHub Actions

Run **Actions → Build Telegram Local Bot API (Linux x86_64) → Run workflow**.
After success, download the `telegram-bot-api-linux-x86_64` artifact, extract the
tar.gz, and upload `telegram-bot-api` into the BotKeep server root beside
`videoconverterbot`.

The Rust bot starts this executable itself when `LOCAL_BOT_API_ENABLED=true`.
Do not start a second Local Bot API process in the Pterodactyl startup command.
After uploading the executable, use:

```sh
chmod +x ./videoconverterbot ./telegram-bot-api && exec ./videoconverterbot
```

Set these BotKeep environment variables (in addition to `BOT_TOKEN`):

```env
LOCAL_BOT_API_ENABLED=true
API_ID=your_api_id
API_HASH=your_api_hash
LOCAL_BOT_API_BIN=./telegram-bot-api
LOCAL_BOT_API_PORT=8081
LOCAL_BOT_API_DIR=./data/local-bot-api
BOT_API_BASE=https://api.telegram.org
```

When local mode is enabled, the Rust program overrides `BOT_API_BASE` and uses
`http://127.0.0.1:8081`. The executable must match the host's Linux architecture.
This workflow builds Linux x86_64. Building the upstream C++ project may take a
while. Running Local Bot API and FFmpeg together in a 512 MB RAM container may
exceed available memory. Local Bot API does not make Telegram transfers unlimited.
