# Production Linux binary builds

The repository workflow `.github/workflows/build-backend.yml` builds the Go
backend for Linux x86_64 (`linux/amd64`). It runs on pushes that change
`backend/**`, and can also be started manually from GitHub Actions using
**Build MoveX production backend** → **Run workflow**.

## Download the built files

1. Open the MOVEX repository on GitHub.
2. Open **Actions** and select **Build MoveX production backend**.
3. Open the latest successful run (green check mark).
4. Scroll to **Artifacts** and download `movex-linux-amd64-<commit>`.
5. Extract the ZIP or TAR.GZ package. The deployable folder contains `movex`
   (the compiled executable) and `start.sh`.

The workflow does not commit generated binaries into Git. Artifacts are retained
for 30 days; download and keep a copy if you need it later.

## Build target and checks

- Target: Linux x86_64 / amd64.
- Go toolchain: 1.23.x (the module declares Go 1.22).
- `CGO_ENABLED=0`; the project uses the pure-Go SQLite driver in production.
- Runs `go mod tidy`, `go mod verify`, `go test ./...`, builds the executable,
  and runs `movex version` before packaging.
- The package contains no `.env`, credentials, database, Telegram session, or
  user data.

## Host requirements before upload

Confirm the hosting runtime is Linux amd64 and supports running an uploaded
executable or extracted ZIP package with a custom start command. Set the
platform-assigned HTTP port as `PORT` (the application currently reads `PORT`).
Set `DATA_DIR` to a persistent writable mount. Do not use an ephemeral working
folder for `movex.db`, `session.key`, Telegram session files, or backups.

The package is a backend executable, not a complete hosting configuration.
Environment variables, persistent storage, HTTPS, health checks, and any
platform-specific deployment settings must still be configured in the host.
