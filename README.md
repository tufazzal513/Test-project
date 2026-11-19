# Rust Telegram Video Converter Bot

A standalone Telegram bot written in Rust. It receives a video, downloads it to a per-job temporary directory, probes it with FFprobe, tries MP4 remuxing when the streams are compatible, otherwise transcodes to H.264/AAC, validates the MP4, sends it back as a Telegram **document**, then removes the job's source and output files.

## Important limitations

- **100% quality is only possible for a compatible remux.** Remux copies encoded streams without re-encoding. A transcode changes the encoded video/audio and may slightly reduce quality; `CRF 18` is a high-quality setting, not a promise of mathematically identical quality.
- All audio streams are mapped. Text subtitles supported by MP4 (`SubRip`, `ASS/SSA`, WebVTT, `mov_text`) are converted to `mov_text`. Bitmap subtitles such as PGS/VobSub are not included because MP4 `mov_text` cannot represent them. Container metadata and chapters are copied where FFmpeg supports them.
- The first video stream is used. Additional video-angle streams, attachments, and arbitrary data streams are not carried into the MP4.
- Telegram's **hosted Bot API** has file download/upload size limits (commonly 20 MB download and 50 MB upload). For larger files, install a **Local Bot API Server** and set `BOT_API_BASE` to its HTTP endpoint. The local server may return an absolute file-system path from `getFile`; this bot can copy it only when that path is visible inside the bot process. If running in separate containers, mount the Local Bot API file directory read-only into the bot container at the same path. Local Bot API Server has its own documented limits (including a 2 GB upload maximum); this is not unlimited. Confirm current Telegram limits before deployment.
- A 512 MB RAM / 50% CPU container can still be too small for very large or complex inputs. Free disk space, processing time, and the hosting provider's limits apply.
- The bot processes one job at a time. Files are streamed to/from disk; upload uses a streaming multipart body, not a whole-file RAM buffer.

## Requirements

- Rust stable (only if building manually)
- FFmpeg with `libx264` and AAC encoder support, plus FFprobe
- Telegram bot token from `@BotFather`
- For large files: a Local Bot API Server reachable by the bot

## Quick start with Docker

1. Create a Telegram bot with `@BotFather` and copy its token.
2. Copy `.env.example` to `.env` and set `BOT_TOKEN`.
3. For the hosted Telegram API, leave `BOT_API_BASE=https://api.telegram.org` and set `MAX_UPLOAD_MB=45`. Note: hosted Bot API download limit is lower, so large incoming files will not work.
4. For larger files, configure a Local Bot API Server and set `BOT_API_BASE` to its API endpoint. Set `MAX_UPLOAD_MB` to a value no greater than that server's supported limit.
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

## Safety and cleanup behavior

- Each job has a separate directory named from chat/message IDs.
- FFmpeg is invoked with argument arrays, not a shell command string.
- A previous-run recovery pass removes abandoned job directories at startup.
- The output is deleted only after Telegram confirms `sendDocument` success. If upload fails, the job reports failure and the temporary job directory is then cleaned up.
- Do not expose `.env` or the bot token. Rotate the token with BotFather if it leaks.

## Known production hardening still recommended

Before exposing this bot to the public, use `ALLOWED_USER_IDS` or implement a rate/abuse policy, configure disk quotas and monitoring, and test with representative media files. The initial version does not implement a persistent job database, progress percentage, per-user quotas, or cancellation commands. The polling loop is sequential and a process restart loses pending Telegram updates older than Telegram's update retention window.
