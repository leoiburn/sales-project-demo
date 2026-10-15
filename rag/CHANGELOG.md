---
feature: changelog
last_reviewed: 2026-10-15
---

# Changelog

Newest first. Every code change gets an entry: **what** changed, **why**, **how** it works, and
which `rag/` files were updated. Code comments don't replace this. See `AGENTS.md`.

Entry format:

```

## YYYY-MM-DD: short title
- What: ...
- Why: ...
- How: ...
- Files: code paths
- Docs: rag files updated
```

## 2026-10-15: Twenty CRM sync (the sender)

- What: queued conversations (`crm_sync`) are now written to Twenty CRM. The customer becomes a
  Person and the conversation an Opportunity. The same records are updated when the customer, a lead
  or an appointment changes. The CRM panel shows the real status and an "Open in Twenty" link.
- Why: the user installed Twenty (`~/twenty-crm`) and asked for the demo to be connected to it.
  The trigger side (`crm_sync`, email consent) was built in parallel. This is the part that
  actually writes to Twenty.
- How: `twenty::tick` runs in `spawn_worker` after `crm_sweep`. It claims due rows with
  `FOR UPDATE SKIP LOCKED`: new rows, retries, stale `sending` rows, and `sent` rows whose facts
  changed. It re-reads everything from the database and creates any missing custom fields. It then
  upserts over Twenty's REST API, finding records by `automotrixCustomerId` and
  `automotrixConversationId` so nothing is duplicated. Failures retry with backoff (2 → 30 min) and
  park as `failed` after 8 attempts. CLI conversations are skipped. The sender is off without
  `TWENTY_API_KEY`.
- Files: `crates/automotrix/src/twenty.rs` (new), `crates/automotrix/src/lib.rs`,
  `crates/automotrix/src/engine.rs` (`spawn_worker`), `crates/automotrix/src/bin/server.rs`
  (`crm_status`), `web/index.html` (`renderCrm`), `migrations/20261015000002_twenty_sync.{up,down}.sql`,
  `docker-compose.yml`, `.env.example`, `.env` (key)
- Docs: `twenty-sync.md` (new), `README.md`, `crm-sync.md`, `crm-panel.md`, `running.md`,
  `testing.md`, `known-issues.md` (removed "Twenty CRM isn't connected"; added the cancellation and
  deleted-record gaps)

## 2026-10-15: Concept-map explainer page

- What: `docs/storybook/index.html`, a stand-alone interactive page that explains the system in
  simple, sales-friendly words. White page, one-colour line icons, no emoji.
- Why: the user wanted a visual explanation for sales demos, first as a picture book, then redone
  as concept maps.
- How: two clickable concept maps. "How everything connects": inventory, knowledge base (RAG),
  calendar and customer records feed the AI; the customer asks; the AI replies, shows cars and
  prices from the database, books, emails the team, sends ADF to the CRM, hands off, thinks in
  steps (tool loop), with SMS and Google Calendar shown as future skills. "How it stays safe": Laya
  screens incoming messages (it does not check replies), house rules (system prompt), customer-only
  knowledge base, database as the only source of facts, price guardrail on every reply, AI label
  added by code, lead summary checked against the chat, handoff. Each sticker opens a card with
  "in plain words" and "what it means for you"; "Follow a message" animates the path. It doesn't
  call the server.
- Files: `docs/storybook/index.html` (no app code changed)
- Docs: this changelog. Update the page by hand if the features it describes change.

## 2026-10-15: Fix every chat turn failing with a Claude API 400

- What: chat works again. `search_inventory`'s `make`, `model`, `drivetrain` and `color` are now
  plain strings (empty = any) instead of nullable.
- Why: the API rejects tool sets with more than 16 union-typed params ("Schemas contains too many
  parameters with union types"). The color filter (commit `38565a3`) made it 17, so every
  `/api/chat` turn returned "Sorry, something went wrong".
- How: plain `string` with "Empty string for any" in the description. `opt_str` already treats a
  blank as `None`, so the SQL is unchanged. There are 13 union params now. New unit test
  `tools::tests::union_typed_params_stay_within_the_api_limit` fails the build if it goes over 16.
- Files: crates/automotrix/src/tools.rs
- Docs: tools.md, inventory-search.md, known-issues.md (issue removed)

## 2026-10-15: Send the lead profile to the CRM when it qualifies, and record email consent

- What: a conversation is queued for the CRM (Twenty) as soon as an appointment is booked, OR the
  customer gave a name and phone, OR an email plus an explicit yes to emails. Email consent is now
  stored and asked for. The CRM panel shows the three conditions and the queue status.
- Why: the user set this rule for when the lead profile goes from the system to the CRM. The
  database had SMS consent but no email consent.
- How: migration `20261015000001_crm_sync` adds `customers.email_opt_in/_at/_source` (with a
  timestamp check) and the `crm_sync` table (one row per conversation). `engine::crm_sweep` runs on
  every worker tick and inserts qualifying conversations as `pending`. `save_contact_info` gets a
  required non-null `email_opt_in`, and the prompt asks once for email consent. `/api/lead` returns
  `crm.twenty` (rules + `crm_sync` row), drawn by `renderCrm`. The sender that writes to Twenty is
  the Twenty session's part and isn't built yet.
- Files: migrations/20261015000001_crm_sync.{up,down}.sql, crates/automotrix/src/engine.rs,
  crates/automotrix/src/tools.rs, crates/automotrix/src/db.rs, crates/automotrix/src/bin/server.rs,
  web/index.html
- Docs: crm-sync.md (new), crm-panel.md, README.md, database.md, conversation-engine.md, tools.md,
  known-issues.md

## 2026-10-15: CRM panel replaces the Lead panel

- What: the operator's "Lead" card is now a "CRM" card. It shows the connections from the chat to
  the CRM (this chat → Automotrix lead → dealership email, CRM ADF email, SMS → Twenty CRM) with
  the live delivery status of each, followed by the record sent to the CRM (the old lead sheet).
- Why: the user wanted the demo to show how the system connects to the CRM, not only the lead's
  contents.
- How: `/api/lead` now returns `crm` next to `lead`, built by `server.rs:crm_status` from `leads`
  and the `outbox` rows `lead:{id}:v1` / `adf:{id}:v1` (no model involved, returned even when `lead`
  is null). `index.html:renderCrm` draws it. SMS shows "not built yet"; Twenty shows "not connected"
  because no integration exists yet. The old email/CRM/SMS icons in the header were removed.
- Files: crates/automotrix/src/bin/server.rs, web/index.html
- Docs: crm-panel.md (new), README.md, web-chat.md, lead-scoring.md, known-issues.md (also logged
  the 17-union tool-schema 400 that breaks every chat turn, and Twenty not connected)

## 2026-10-15: Full-screen button on the side panels

- What: each operator side panel (Lead profile, Lead, Appointments, Showroom) has a ⛶ button in its
  header that opens it full screen.
- Why: the panels are small during a sales demo; the user asked for a way to blow them up.
- How: the script adds a `.fs` button to every `.card` header. It uses the Fullscreen API and the
  `.max` class (fixed, covers the window) as a fallback for iPhone. ✕, Esc or leaving browser full
  screen restores it. Showroom ring and detail photo get bigger in full screen.
- Files: `web/index.html`
- Docs: `web-chat.md`

## 2026-10-15: Back to Docker; start and stop scripts

- What: the project runs on Docker again. `scripts/up.sh` now uses `docker compose`, new
  `scripts/down.sh`, `init: true` on `app`. Removed the Podman user unit `automotrix.service`.
- Why: under rootless Podman the page showed only the chat. rootlessport made every request come
  from the container's own IP, so `is_operator` never matched and the lead, appointments and
  showroom panels were hidden. The user also wants to start and stop the demo at will.
- How: `up.sh` exports the current LAN IP and runs `docker compose up -d --build`; `down.sh` runs
  `docker compose down` and keeps the volumes. `init: true` forwards SIGTERM, so stopping takes about
  2 s instead of a 10 s SIGKILL. The `docker` service is enabled with `ip-forward-no-drop` so the
  Kali VM keeps internet.
- Files: `scripts/up.sh`, `scripts/down.sh`, `docker-compose.yml`, `/etc/docker/daemon.json`
  (host), `.gitignore` (`.env.bak*`)
- Docs: `running.md`, `server-and-security.md`, `known-issues.md` (removed the Podman entry)

## 2026-10-15: Start script with automatic LAN IP and autostart at login

- What: added `scripts/up.sh` and the user unit `scripts/automotrix.service` (enabled, along with
  `podman.socket`). `.env` no longer hardcodes the LAN IP.
- Why: the page kept failing at startup. The laptop's DHCP IP changed from `192.168.12.108` to
  `.197`, but `.env` pinned `PUBLIC_HOSTNAME`/`PUBLIC_BASE_URL` to `.108`, so access by IP timed out
  and the certificate and email links named a dead address. After a reboot nothing restarted the
  containers, so the browser showed only its own error page.
- How: `up.sh` reads the current IP (`ip route get`), exports `PUBLIC_HOSTNAME` and
  `PUBLIC_BASE_URL` (these override `.env`), starts `podman.socket` and runs `podman compose up -d`.
  systemd runs it at each login. `.env` keeps an empty `PUBLIC_HOSTNAME` and a localhost base URL
  as the fallback (the old file is in `.env.bak-20261015`).
- Files: `scripts/up.sh`, `scripts/automotrix.service` (removed later the same day), `.env` (not committed)
- Docs: `running.md`, `server-and-security.md`, `known-issues.md` (new rootless operator-view bug)

## 2026-10-10: Knowledge base created

- What: added `rag/` with one file per feature, this changelog, and `AGENTS.md`/`CLAUDE.md`
  requiring every change to be documented here.
- Why: so any person or AI agent can find how a feature works without reverse-engineering the code.
- How: written from a full read of the code at commit `38565a3` plus the uncommitted working tree
  (VIN spec enrichment, `OPERATOR_IPS`). Bugs found along the way are listed in
  `known-issues.md`; nothing in the code was changed.
- Files: none (documentation only)
- Docs: all of `rag/`

## Before 2026-10-10 (from git history, summarized)

- Uncommitted: VIN spec enrichment (`enrich.rs`, migration `20261002000002`), `OPERATOR_IPS`.
- `38565a3`: the bot can search inventory by color (migration `20261002000001`).
- `176d9ae`: Laya screening became optional through `LAYA_URL`.
- `a36620e`: lead channel icons, appointment reminder and follow-up boxes.
- `6c243e6`: lead sheet, intent scoring, booking form, showroom ring, unique car photos.
- 2026-09-23: tool schemas switched to `anyOf` for optional enums (the API rejected
  `type: [string, null]` next to an `enum`); all Python and Bash scripts ported to Rust.
