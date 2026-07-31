# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
# Run in development (loads .env if present)
cargo run

# Run with a custom platform name
APP_NAME="Mi Nube" cargo run --release

# Build release binary
cargo build --release
```

The server starts on port `3000` by default. Dashboard: `http://localhost:3000`. Default credentials: `admin` / `admin123`.

Environment configuration is read from `.env` (copy from `.env.example`). Key variables:

| Variable | Default | Purpose |
|---|---|---|
| `MYPAAS_DATA_DIR` | `/var/lib/mypaas` | Root for all runtime data |
| `MYPAAS_PORT` | `3000` | Main HTTP port |
| `MYPAAS_DOMAIN` | `localhost` | Base domain for app subdomains |
| `MYPAAS_APP_PORT_START/END` | `8100`/`8999` | Port range for deployed apps |
| `MYPAAS_SUSPEND_TIMEOUT_MINS` | `10` | Idle minutes before auto-suspend |
| `APP_NAME` | `RustPaaS` | Platform display name across UI |
| `RUST_LOG` | — | Log filter (e.g. `mypaas=debug`) |

No `DATABASE_URL` is needed at compile time — SQLx uses runtime queries throughout.

## Architecture

RustPaaS is a **single monolith binary** (`mypaas`) that serves three concerns over one HTTP port:

1. **Dashboard & Auth** — Askama-rendered HTML pages at `/`, `/login`, `/register`, `/dashboard/**`
2. **REST API** — JSON endpoints at `/api/**` (deploy, CRUD, backups, env management)
3. **Reverse proxy** — Everything else falls through to `proxy_handler` in `runner/proxy.rs`, which reads the `Host` header, extracts the subdomain, finds the matching project in SQLite, and forwards the request to that project's local port

### Request routing

The Axum router registers specific paths first; unmatched paths hit the `.fallback(proxy_handler)`. The proxy checks if the `Host` header contains a subdomain of `MYPAAS_DOMAIN`. If it does, it wakes the process if needed (scale-from-zero) and forwards the request to `http://127.0.0.1:{project.port}`.

### Process lifecycle

When an app is deployed (`POST /api/deploy`):
1. A port is allocated from the `8100–8999` range (checked against DB)
2. The binary is written to `{data_dir}/apps/{name}/bin/{name}`
3. A private S3 server (s3s-fs) starts on port `project.port + 10000`
4. The binary is spawned with injected env vars: `DATABASE_URL`, `S3_ENDPOINT`, `S3_BUCKET`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `PORT`, `APP_NAME`
5. stdout/stderr go to `{data_dir}/apps/{name}/app.log`
6. A custom `{data_dir}/apps/{name}/data/.env` file can override any env var except `PORT`

A background tokio task checks every 60s for processes idle longer than `MYPAAS_SUSPEND_TIMEOUT_MINS` and kills them (`ProjectStatus::Suspended`). The proxy reactivates suspended processes on next request with a 500ms startup delay.

### Shared state (`AppState`)

Defined in `src/api/deploy.rs`, shared across all Axum handlers:
- `config: Config` — loaded once from env at startup
- `db: SqlitePool` — SQLx pool, max 5 connections
- `process_manager: Arc<Mutex<ProcessManager>>` — tracks live `Child` processes and S3 server handles

### Data directory layout

```
{MYPAAS_DATA_DIR}/
├── paas.db                         # PaaS-internal SQLite (users, projects, buckets, sessions)
├── apps/{name}/
│   ├── bin/{name}                  # Deployed executable
│   ├── data/app.db                 # App's own SQLite (injected via DATABASE_URL)
│   ├── data/.env                   # Custom env overrides (editable via dashboard)
│   ├── data/.s3_secret             # Generated per-project S3 credentials (persisted)
│   └── app.log                     # Captured stdout+stderr
└── storage/buckets/{project_id}/   # Per-project S3 storage (s3s-fs directories)
```

### Authentication

Two methods accepted by `POST /api/deploy` and protected endpoints:
- **Session cookie** (`rustpaas_session`) — set after login, expires 7 days
- **Bearer API key** — `Authorization: Bearer paas_sk_<uuid>`, stored per user in `users.api_key`

Dashboard routes use the `AuthUser` extractor (`src/dashboard/auth.rs`), which redirects to `/login` on failure. The `OptionalAuthUser` extractor is used where auth is optional (landing page).

### Templates & i18n

HTML templates live in `templates/` and are compiled into the binary by Askama. Template structs are defined in `src/dashboard/templates.rs`. The `Lang` extractor (`src/dashboard/i18n.rs`) reads `Accept-Language` and returns a translation struct (`T`) supporting English and Spanish. The `pub mod filters {}` declaration in `src/main.rs` is required by Askama and must not be removed.

Static assets (`static/styles.css`, `static/app.js`) are served from disk via `tower-http ServeDir` and not embedded.

### Database migrations

Migrations are embedded via `sqlx::migrate!("./migrations")` and run automatically on startup. New migrations go in `migrations/` with the naming pattern `NNN_description.sql`.
