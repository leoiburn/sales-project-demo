---
feature: running
files: [docker-compose.yml, Dockerfile, .env.example, crates/automotrix/src/bin/cli.rs, scripts/up.sh, scripts/down.sh]
status: committed
last_reviewed: 2026-10-15
---

# Running the project

## Docker services

| Service | Image | Reachable at | Purpose |
|---|---|---|---|
| `db` | `pgvector/pgvector:pg17` | `localhost:5432` | Postgres with pgvector; the data lives in volume `automotrix_pgdata` |
| `mailpit` | `axllent/mailpit` | SMTP on 1025, UI at http://localhost:8025 | Fake inbox where every email shows up |
| `stt` | `onerahmet/openai-whisper-asr-webservice` | internal only | Speech to text |
| `tts` | `ghcr.io/remsky/kokoro-fastapi-cpu` | internal only | Text to speech |
| `app` | built from `Dockerfile` | https://localhost:8443 | Runs `load`, then `server` |

The Dockerfile builds `server` and `load` with `rust:1.98-slim-bookworm` and runs them on
`debian:bookworm-slim` with only CA certificates; every TLS path uses rustls. The app reads its data
from `/app` (`AUTOMOTRIX_ROOT`).

## Environment variables (`.env`, copied from `.env.example`)

| Variable | Purpose |
|---|---|
| `POSTGRES_USER/PASSWORD/DB/PORT/HOST`, `DATABASE_URL` | Database (inside Docker the app gets its own `DATABASE_URL`) |
| `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL` | Claude (default `claude-haiku-4-5`) |
| `EMAIL_TRANSPORT`, `SMTP_URL`, `POSTMARK_TOKEN`, `EMAIL_FROM` | Mail, see [outbox-and-email.md](outbox-and-email.md) |
| `HTTPS_PORT`, `PUBLIC_HOSTNAME`, `PUBLIC_BASE_URL` | Server port, an extra certificate name (your LAN IP, for phones), links in emails |
| `LAYA_URL`, `LAYA_API_KEY` | Message screening, see [laya-screening.md](laya-screening.md) |
| `OPERATOR_IPS` | Extra operator IPs |
| `TWENTY_API_KEY`, `TWENTY_URL`, `TWENTY_PUBLIC_URL` | Twenty CRM sync; empty key = off. See [twenty-sync.md](twenty-sync.md) |
| `STT_URL`, `TTS_URL`, `STT_MODEL` | Speech services |

## Starting and stopping (`scripts/up.sh`, `scripts/down.sh`)

Use **Docker only.** Podman is not supported: under rootless Podman every request reaches the app
from the container's own IP, so `is_operator` never matches and localhost gets only the chat
(no lead profile, appointments or showroom). Docker port publishing keeps the gateway/real IPs
that `is_operator` relies on (see [server-and-security.md](server-and-security.md)).

| Command | What it does |
|---|---|
| `scripts/up.sh` | Checks Docker is running, reads the current LAN IP (`ip -4 route get 1.1.1.1`, the `src` field), exports `PUBLIC_HOSTNAME=<ip>` and `PUBLIC_BASE_URL=https://<ip>:<HTTPS_PORT>`, then `docker compose up -d --build` |
| `scripts/down.sh` | `docker compose down`: removes containers and network, **keeps the volumes** (database, certificate, Whisper model). Never add `-v`. |

Exported shell variables override `.env` in compose, so the certificate and email links always use
the current IP. `.env` leaves `PUBLIC_HOSTNAME` empty and sets `PUBLIC_BASE_URL=https://localhost:8443`
as the fallback for a plain `docker compose up`.

**Why:** the laptop gets its IP by DHCP. `.env` had `192.168.12.108` hardcoded, and the IP later
became `.197`, so access by IP timed out.

Nothing starts the stack by itself: you start and stop it when you want. The `docker` service is
enabled at boot, with `/etc/docker/daemon.json` = `{"ip-forward-no-drop": true}` so Docker doesn't
set the FORWARD policy to DROP (that used to cut internet to the local Kali VM). Containers left
running at shutdown come back at boot (`restart: unless-stopped`); containers removed by `down.sh` don't.

`app` has `init: true`. The server runs without a SIGTERM handler, so as PID 1 it ignored the stop
signal and was SIGKILLed after 10 s. With the init process `down.sh` takes about 2 s.

URLs: https://localhost:8443 (it's **https** and port **8443**; `http://localhost` has nothing
listening), `https://<LAN IP>:8443` for phones, and http://localhost:8025 for Mailpit.

## Everyday commands

```bash
docker compose up -d                 # start everything
docker compose up -d app             # pick up .env changes (restart does NOT reload .env)
docker compose up -d --build app     # pick up code changes
cargo run -p seed --bin reset_db     # wipe and reload the database
cargo run --bin cli [-- --session ID]  # chat from the terminal; /worker flushes email, /quit exits
cargo test --workspace               # all tests
```

## The CLI

`cli` runs the same `engine::turn` on the `cli` channel. A session id (random, or `--session ID` to
resume) maps to one conversation. `/worker` runs lead delivery and one outbox pass, so you can watch
emails being built. It prints shown cars as `[stock] label - price - miles`.

## Gotchas

- Compose project name is `sales-project-demo`, so volumes are `sales-project-demo_automotrix_*`.
- `cargo` lives in `~/.cargo/bin`, which may not be on `PATH` (noted 2026-09-23).
- Test conversations `test-claude-check-0001` to `0003` were left in the database on 2026-09-23.
