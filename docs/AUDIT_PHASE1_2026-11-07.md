# MOVEX Deep Audit - Phase 1 (2026-11-07)

## Scope inspected

- Go backend package layout, startup path, configuration validation, HTTP middleware, admin routes, login/session/CSRF checks, access logging, and audit logging.
- GitHub Actions backend and Pages workflows.
- Frontend JavaScript syntax, local HTML asset references, and frontend Node.js tests.
- Existing `PROGRESS.md` and `MOVEX_WORK_PLAN.md` status.

## Changes in Part 38

- Hardened the admin same-origin check for browsers that send `Origin: null`.
- A `null` Origin is not accepted by itself. It now requires `Sec-Fetch-Site: same-origin` and a parseable `Referer` whose authority matches the request's expected host (the trusted forwarded host when `TRUST_PROXY=1`).
- Malformed, missing, cross-site, same-site-only, user-info, and lookalike-host Referer values are rejected by regression tests.
- Changed the admin `Referrer-Policy` from `no-referrer` to `same-origin`, so the same-origin Referer needed by the strict null-Origin fallback can be sent to this same site while no Referer is sent to other origins.
- Login diagnostics now include only the Referer origin (scheme + authority), never its path or query string. This helps diagnose proxy/browser behavior without recording potentially private URL data.
- Existing login/session CSRF tokens remain required. This change does not disable CSRF checks and does not make `Origin: null` universally trusted.

## Validation completed in this environment

- `node --test frontend/tests/*.test.cjs`: PASS, 21/21.
- `node --check` for all frontend JavaScript and CJS test files: PASS.
- Local HTML `src`/`href` references: 0 missing local references.
- `go test -tags nodriver` for these 11 packages: PASS:
  - `internal/admin`
  - `internal/auth`
  - `internal/backup`
  - `internal/config`
  - `internal/db`
  - `internal/httpx`
  - `internal/presence`
  - `internal/replica`
  - `internal/repo`
  - `internal/server`
  - `internal/taxonomy`
- `go vet -tags nodriver` for the same 11 packages: PASS.
- The new admin origin tests, privacy-safe Referer-origin logging test, and same-origin Referrer-Policy test are included in `internal/admin`'s `nodriver` test path.

## Blocked / not verified

- Full `go test ./...` and the production executable build are not verified here. The archive has no `go.sum`; `go mod tidy` is the intended workflow step, but module downloads fail in this environment because `proxy.golang.org` DNS/network access is unavailable.
- `internal/telegrammtproto` and `cmd/movex` cannot be compiled here because `github.com/gotd/td` is not cached. The `nodriver` test path also intentionally excludes the production SQLite driver and some integration tests.
- No live Firebase login, browser-origin behavior, Botkeep reverse-proxy request, Telegram OTP/2FA, Telegram backup/restore, or main/worker network integration was exercised.
- This is a source-level compatibility fix for the observed `Origin: null` case. The Botkeep login outcome must be confirmed from the next deployed build and its request logs.

## Logging findings

- The HTTP access middleware already writes one structured request log for every handled HTTP request: request ID, method, path (without query string), status, response bytes, duration, client IP, and authenticated UID when available.
- The admin audit table records selected administrative mutations, not every page view or every internal operation. Access logs and audit logs serve different purposes and should remain separate.
- Never add passwords, cookies, authorization headers, Firebase ID tokens, Telegram codes/passwords, API hashes, or full Referer URLs to logs.

## Additional audit findings for follow-up

- `PROGRESS.md` marks P13/P14 as partial and P15/P16 as partial or integration-pending. P17-P19 frontend migration still needs a complete browser-level review; P20 delivery/hardening is not complete.
- The active admin route set includes a user-status mutation endpoint. Confirm whether this is still desired before changing it; older project requirements indicated that user freeze/delete controls were not wanted.
- Full production readiness is not certified by this phase. The next audit phase should review every admin form/action against its handler, all API authorization/error paths, database migration/restore invariants, Telegram lifecycle, worker replication failure recovery, and deployment configuration.

## Deployment notes

- Keep `ALLOWED_ORIGINS=https://tufazzal513.github.io` for the GitHub Pages frontend. Do not add the backend URL as a CORS origin just to fix the Admin Panel's same-origin login.
- Keep `TRUST_PROXY=1` only when the hosting router is the trusted reverse proxy and overwrites forwarded headers.
- Do not delete or replace the persistent `DATA_DIR`, SQLite database, Telegram session, or session key while deploying this source update.
