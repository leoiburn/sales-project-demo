---
feature: server-and-security
files: [crates/automotrix/src/bin/server.rs, docker-compose.yml, .env.example]
status: committed
last_reviewed: 2026-10-15
---

# HTTPS server, routes and operator access

## What it does

`server` is an axum app over rustls. It serves the web chat and its JSON API on `0.0.0.0:8443`, and
runs the background worker in the same process.

## Routes

| Route | Method | Who | Purpose |
|---|---|---|---|
| `/` | GET | everyone | The chat page. Non-operators get `<body class="chat-only">`. |
| `/api/chat` | POST | everyone | One chat turn |
| `/api/history` | GET | everyone | Past messages for a session |
| `/api/health` | GET | everyone | `{db, llm_configured, model, mailer}` |
| `/api/gallery` | GET | everyone | The showroom ring |
| `/api/slots` | GET | everyone | Open times for the booking form |
| `/api/book` | POST | everyone | Booking form submit |
| `/api/stt` | POST | everyone | Speech to text relay (10 MB limit) |
| `/api/tts` | POST | everyone | Text to speech relay (text up to 4000 characters) |
| `/api/calendar` | GET | **operator** | Calendar panel |
| `/api/lead` | GET | **operator** | Lead profile and scoring |
| `/cars/*` | GET | everyone | Car photos served from the repo |

## Who counts as an operator (`is_operator`)

A request is from the operator if the peer IP is **loopback**, **the default gateway** (read from
`/proc/net/route`, which is how the host's own browser shows up through Docker port publishing), or
listed in **`OPERATOR_IPS`** (comma-separated). Other operator-only requests get 403. LAN clients,
such as a phone on the same Wi-Fi, keep their real IP and get the customer view.

`OPERATOR_IPS` was added for rootless Podman, where it never worked
(see Gotchas); under Docker it's only needed for extra machines.

## TLS certificates

On start, `ensure_cert()` generates a **self-signed** certificate (rcgen) for `localhost`,
`127.0.0.1`, the machine's outward LAN IP (found by "connecting" a UDP socket to 8.8.8.8, which sends
no packet) and `PUBLIC_HOSTNAME`. It's stored in `certs/` (a Docker volume), so the browser warning
is accepted once. It's regenerated when that list of names changes. Inside a container the
"outward LAN IP" is the container's own address, so the host's LAN IP only gets in through
`PUBLIC_HOSTNAME`, which `scripts/up.sh` sets to the current IP on each start (see [running.md](running.md)).

## Input checks

Session ids must be 8–64 characters of `[A-Za-z0-9-]`. Chat text must be 1–2000 characters. The
booking form needs a name (up to 100 characters) and a phone. Both rustls users (sqlx and
axum-server) are pinned to the `ring` crypto provider at startup.

## Startup

Load `.env`, connect to Postgres, build the LLM client (with no key the chat answers with a setup
message), load templates (HTML auto-escaped), choose the mail transport, load the summary schema,
spawn the worker, serve.

## Gotchas

- Don't run it under Podman: rootless Podman hides the client IP (every peer is the container's own
  IP), so nobody is an operator. Docker keeps the gateway IP for the host browser.
- Behind a reverse proxy, every request would come from the proxy's IP, and that could make
  everyone an operator.
- There's no rate limiting on any route, including the ones that call the LLM or the speech models.
