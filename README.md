# Shitverter Bot

## Introduction
This bot waits for video files in chats and converts them to `.mp4` using FFmpeg.

## Requirements
- Rust
- FFmpeg
- Docker (optional)
- [Just](https://github.com/casey/just) (command runner)

## Installation and Running

### Using Just (recommended)
```bash
# Show available commands
just

# Rebuild the project from scratch and run in docker
just rebuild

# Just run the container (without rebuilding)
just run
```

### Using helper scripts

```bash
# Rebuild image + run container
./rebuild.sh

# Run existing image
./run.sh
```

### Local Setup (alternative)

```bash
cargo build --release
./target/release/converter-bot
```

### Using Docker directly (alternative)

```bash
# Build production image (multi-stage Dockerfile; final stage is the runtime image).
docker build -t shitverter:latest .

# Run it.
docker run -d --env-file .env --name my_shitverter_container shitverter:latest

# Optional: build the reusable toolchain image (Rust + musl) for local builds/debugging.
docker build --target build-env -t shitverter-buildenv:latest .

# Optional: pin the Rust toolchain image for reproducible builds.
docker build --build-arg MUSLRUST_TAG=1.93.1-stable-2026-02-23 -t shitverter:latest .
```

## Configuration

Set the following environment variables:

* `TELOXIDE_TOKEN`: Your Telegram Bot Token.
* `TELOXIDE_API_URL` (default: `https://api.telegram.org`) — Bot API base URL.
* `LOCAL_BOT_API_BIN` (optional) — path to the official Local Bot API executable.
  When set, the Rust bot starts and supervises it in the same container.
* `TELEGRAM_API_ID` and `TELEGRAM_API_HASH` — required when
  `LOCAL_BOT_API_BIN` is set.
* `LOCAL_BOT_API_PORT` (default: `8081`) — loopback HTTP port for the local API.
* `LOCAL_BOT_API_TEMP_DIR` (optional) — Local Bot API server's temporary directory.
* `TELEGRAM_LOCAL_FILES_DIR` (optional) — shared absolute cache directory for a
  local Bot API server started with `--local`. Requires a non-cloud
  `TELOXIDE_API_URL`; the directory must exist and be writable by the bot so
  consumed media cache files can be deleted safely.
* `TELEGRAM_REQUEST_TIMEOUT_SECONDS` (default: `300`) — HTTP request timeout.
  Local `getFile` downloads the media before responding, so allow enough time
  for the largest accepted input and output uploads.
* `ALLOWED_TELEGRAM_USER_ID`: required in `--drain-queue` mode; only updates
  whose sender has this numeric Telegram user ID may reach the download and
  conversion path. All other updates are acknowledged and silently discarded.
* `RUST_LOG`: Log level filter for the bot output (example: `RUST_LOG=info`, default: `info`).
* `USER_DAILY_LIMIT` (default: `10`) — maximum conversions per user per UTC day.
* `GLOBAL_DAILY_LIMIT` (default: `50`) — maximum conversions for the whole bot per UTC day.
* `MAX_INPUT_BYTES` (default: `104857600`, 100 MiB) — maximum downloaded source-video size.
* `MAX_OUTPUT_BYTES` (default: `49000000`) — maximum converted-video size before upload.
* `TMPDIR` (optional) — disk-backed temporary workspace directory. It is created
  at startup and only this bot's `shitverter-*` job folders are cleaned.




## BotKeep / Pterodactyl deployment

This repository can run the official Local Bot API Server and the Rust bot in
one BotKeep container. Build the BotKeep artifact from **Actions → Build
BotKeep deployment → Run workflow**. Upload `converter-bot` and
`start-botkeep.sh` from that ZIP, and also upload the Linux x86_64
`telegram-bot-api` executable produced by the separate Local Bot API build.
Copy `.env.example` to `.env`, then fill in the existing bot token and your
`TELEGRAM_API_ID` / `TELEGRAM_API_HASH` from `my.telegram.org`.

Set the BotKeep Startup command to:

```sh
sh ./start-botkeep.sh
```

The Rust process starts Local Bot API on `127.0.0.1:8081`, waits for Telegram
authentication, then starts polling. It keeps the API process alive and stops
it when the bot exits. `data/local-bot-api` holds Telegram session/cache data;
`data/jobs` holds temporary conversion files. Do not manually delete the
Local Bot API folder while the bot is running.

The bot always runs one conversion at a time. Remux is attempted first; only
incompatible streams are transcoded. Converted MP4 files are sent as documents.
Uploads are retried at most three times. Consumed Telegram `file_*` cache files
are deleted after copying, abandoned cache files and `shitverter-*` job folders
are cleaned at startup, and normal job files are removed when each job ends.

## MP4 conversion behavior

The converter probes the input with FFprobe and first attempts a stream-copy
remux into MP4. A successful remux preserves the encoded audio/video streams
without re-encoding. If the streams are not MP4-compatible, it falls back to
H.264/AAC transcoding using one FFmpeg thread. This fallback is high quality but
cannot be guaranteed bit-for-bit identical to the source. All audio tracks are
mapped; supported text subtitle tracks are converted to MP4 `mov_text`; global
metadata and chapters are carried over where FFmpeg supports them. Bitmap
subtitles such as PGS/DVD subtitles are not representable as MP4 text subtitles
and are omitted. The resulting MP4 is sent as a Telegram document. Upload is
retried at most three times; failed jobs are not re-queued automatically.

## Files larger than 20 MB

Telegram can accept a video in a chat while its cloud Bot API refuses to let a
bot download it: cloud `getFile` is limited to **20 MB**. Raising
`MAX_INPUT_BYTES` alone cannot change that transport limit.

For the Docker deployment, add `TELEGRAM_API_ID` and `TELEGRAM_API_HASH` to the
GitHub `production` environment secrets and run **Deploy** with
`enable_local_api=true`. It builds and starts the official API container,
performs the session migration, and configures inputs up to **2 GiB** and outputs
up to **2000 MB**. See [activation and recovery](deploy/README.md#enable-large-files-from-github-actions).

Run the official [Telegram Bot API server](https://github.com/tdlib/telegram-bot-api)
with `--local`, then configure Shitverter to use it:

```dotenv
TELOXIDE_API_URL=http://telegram-bot-api:8081
TELEGRAM_LOCAL_FILES_DIR=/var/lib/telegram-bot-api
TELEGRAM_REQUEST_TIMEOUT_SECONDS=1800
MAX_INPUT_BYTES=2147483648
MAX_OUTPUT_BYTES=2000000000
```

Mount the Bot API data directory at the **same absolute path** in both containers;
the bot needs access to read source files and remove only consumed per-bot media
cache files. Local `getFile` returns an absolute path, and
Shitverter streams a bounded copy into its private conversion workspace. It
rejects paths outside the configured directory (including symlink escapes),
then removes the consumed per-bot `file_*` cache file after the copy succeeds.
On startup it removes only stale `file_*` media files in this bot's cache folder,
preserving TDLib session/database files and all other entries. Temporary job
directories are deleted after processing and abandoned `shitverter-*` workspaces
are removed safely on the next startup. Set `TMPDIR` to a disk-backed directory
(for example `./data/jobs`) if the container's default `/tmp` is memory-backed.
The API endpoint alone does not provide an HTTP file server.

The local API removes the cloud download limit and supports uploads up to
**2000 MB**, but the bot's own resource limits remain independent: the defaults
still accept at most **100 MiB** input and **49,000,000 bytes** output. Increase
both limits deliberately when needed, and provision storage for the API cache,
the input copy, and the converted output. The advertised input size is checked
before `getFile`; returned metadata and the actual bytes copied are checked too.

The server requires your Telegram `api_id` and `api_hash`. To migrate an existing
bot, stop its old update consumer and call the cloud API's `logOut` before
starting it against the local endpoint. Telegram prevents returning to the cloud
API for 10 minutes after `logOut`. Both long-running and `--drain-queue` modes use
the same configuration; an ephemeral worker still needs access to the shared
directory. See [deployment instructions](deploy/README.md) for the container
setup and [Telegram's local API documentation](https://core.telegram.org/bots/api#using-a-local-bot-api-server)
for the transport limits and migration requirements.

## One-shot queue mode

Released binaries also support a bounded, stateless queue-drain mode for ephemeral
CI workers:

```bash
converter-bot --drain-queue
```

It consumes Telegram `getUpdates` sequentially, handles at most one media job and
100 updates by default, then exits. Override those bounds with
`MAX_CONVERSIONS_PER_RUN` and `MAX_UPDATES_PER_RUN`. Do not run this mode while the
long-running bot or a webhook is consuming updates for the same token.

`ALLOWED_TELEGRAM_USER_ID` is mandatory in queue mode. Authorization is checked
before Telegram file metadata is requested or media is downloaded. Messages
without a sender user (including channel posts) are rejected.

Queue delivery is at-least-once: a crash after sending a result but before
acknowledging its update can create a duplicate. Pending Telegram updates are not
a durable replacement for a database-backed queue.

If a local `.env` file exists, `run.sh` and `rebuild.sh` automatically pass it to
`docker run` using `--env-file .env`.

## Usage

Send a video file (for example, `.webm`, `.mkv`, `.mov`) to the chat with the bot, and it will send a converted `.mp4` file and delete the original message.
Also shows tg ID's of new members.

## Rate limits

The bot logs quota decisions and resets counters at UTC midnight. Limits are held only in memory, so a restart resets them.

## Contributing

Contributions are welcome. Please send pull requests.

The checked-in toolchain follows Rust's `stable` channel. Fresh CI workers
therefore pick up each stable Rust release. Dependabot checks Cargo, Docker, and
GitHub Actions dependencies weekly; patch and minor updates merge automatically
only after every reported check succeeds. Major updates remain open for review.

## CI and production deployment

GitHub Actions validates formatting, tests, lints, RustSec advisories, and the
production Docker build for pull requests and `main`. A separate manually
promoted GitHub Actions workflow can publish an immutable GHCR image and deploy
it to the bot VM without copying the Telegram token to GitHub. See
[`deploy/README.md`](deploy/README.md) for VM bootstrap and the required GitHub
environment secrets.
