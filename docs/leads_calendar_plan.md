# Leads, transcripts, email and calendar — Milestone 0 (discovery + plan)

Date: 2026-09-21
Status: **built.** Milestones 1-4 are implemented and tested; see section 7 for
what was decided and what is still open. Sections 1-6 are the original discovery,
kept as written.

---

## 1. What already exists

### Infrastructure

| Piece | State |
|---|---|
| `docker-compose.yml` | `pgvector/pgvector:pg17`, named volume, `pg_isready` healthcheck, credentials from `.env`. One service: `db`. |
| Migrations | Two reversible pairs in `/migrations`, sqlx-cli naming, run with `sqlx migrate run`. `_sqlx_migrations` is live in the database. |
| `scripts/reset_db.sh` | empty volume → `up` → healthcheck → migrate → load → verify, one command. |
| Extensions installed | `plpgsql`, `vector 0.8.6`. **`btree_gist` is not installed** — Milestone 1's EXCLUDE constraint needs it. |

### Schema (8 tables + 1 view, all live)

`dealers`, `vehicles`, `vehicle_internal`, `vehicle_photos`, `trim_specs`,
`documents`, `doc_chunks`, plus the `v_inventory` view.

Conventions already established and worth keeping:

- Money is `bigint` cents everywhere. No float touches a price.
- Every tenant table carries `dealer_id`; all timestamps are `timestamptz`.
- Every enum-like column is held by a `CHECK`, not a Postgres enum type.
- Ids are **uuid v5** over a fixed namespace (`seed::uid`), derived from a
  natural key. That is what makes the loader idempotent.

Loaded data: 1 dealer (Automotrix), 69 vehicles, 483 photos, 20 `trim_specs`,
21 documents, 251 `doc_chunks`.

### SQL-side guardrails (these exist and are the pattern to follow)

`migrations/20260921000002_search.up.sql`:

- `match_documents(dealer, vector(768), limit, min_score)` — cosine search that
  filters `audience = 'customer'` **inside the function**, so the bot's own
  operating rules cannot be retrieved into a customer answer, and applies a
  relevance floor (default 0.45) below which it returns nothing.
- `search_inventory(dealer, body_type, make, ..., status)` — typed filtering
  only, never similarity, defaulting to `status = 'available'`.
- `doc_chunks` carries `risk` and `disclaimer` columns with
  `check (risk <> 'high' or disclaimer is not null)`.

### Rust

- Workspace root `Cargo.toml`, one member: `crates/seed`.
- `crates/seed` — binaries `load` and `verify`, plus `lib.rs` (uuid v5 ids,
  COPY escaping, repo-root resolution, `database_url()`).
- Dependencies already in the tree: `sqlx 0.9` (postgres, uuid, chrono, json,
  macros, rustls), `pgvector 0.4`, `tokio`, `serde`, `serde_json`, `uuid`,
  `anyhow`, `chrono`, `csv`, `dotenvy`, `rand`.
- Toolchain: rustc 1.98.1, `sqlx-cli` installed.

### Data generation (at the time of discovery: Python; since ported to Rust in `crates/datagen`)

`gen_inventory` (synthetic units) and `build_corpus`
(chunking + `BAAI/bge-base-en-v1.5` embeddings). Nothing that touches the
database at runtime.

---

## 2. What is missing

The brief's CONTEXT describes several things as already present. They are not.
This is the most important finding in this document.

| The brief assumes | Reality |
|---|---|
| "The LLM is Claude Haiku via the Messages API, using tools" | **No LLM client exists.** No HTTP client, no `ANTHROPIC_API_KEY` handling, no model config. Zero matches for `anthropic`/`claude`/`haiku` anywhere in the Rust code. |
| A tool loop | **Does not exist.** |
| A CLI / test harness | **Does not exist.** There is no interactive entry point at all; `load` and `verify` are batch binaries. |
| Milestone 2: "Run the **existing** price/number guardrail on the summary text" | **Does not exist.** The guardrails that exist are SQL-side (audience filter, relevance floor, risk/disclaimer constraint, available-only inventory). There is no text-scanning guardrail that checks numbers or prices against the database. |
| `guardrail_events` ("create only if missing") | Missing. |

Also missing, as expected: `btree_gist`, every table in Milestone 1, email of any
kind, Mailpit, templates, the outbox, the calendar.

### Consequence

Milestone 4's four LLM tools cannot be exercised without a tool loop and a CLI,
and Milestone 2's summary validation cannot "run the existing guardrail" because
there is none. Both have to be built as part of this work. I read that as
in-scope-by-necessity rather than scope creep, but it is a real addition to what
the milestones describe, and it is worth you knowing before I start.

---

## 3. Plan

### Crate layout

Two new crates, not five. The work splits cleanly into "a library of things the
bot does" and "ways to run it":

```
crates/
  seed/           (exists) load + verify binaries
  automotrix/     NEW lib: db access, transcript, summary, guardrails, adf,
                  email, calendar, tools
    src/bin/cli.rs      test harness — drives a conversation from the terminal
    src/bin/worker.rs   outbox sender (tokio task, also runnable standalone)
```

One lib crate keeps the compile graph small and lets the modules share the
`sqlx::PgPool`, the dealer settings cache and the error type without a web of
inter-crate dependencies. If a module later needs to ship separately, splitting
it out is mechanical; splitting early is not free.

Modules inside `automotrix`:

| Module | Job |
|---|---|
| `db` | pool, `dealer_settings`, typed row structs |
| `guardrail` | the price/number scanner + `guardrail_events` writer |
| `llm` | Messages API client, tool loop, tool definitions |
| `transcript` | text and JSON renderers, built from `messages` |
| `summary` | the one Haiku call, schema config, validation |
| `adf` | ADF 1.0 via `quick-xml` |
| `email` | `EmailSender` trait, SMTP and HTTP impls, templates |
| `outbox` | enqueue + the worker loop |
| `calendar` | `CalendarProvider`, `LocalCalendar`, Google stub |
| `tools` | the four LLM tools, each validating against the database |

### Milestone 1 — data model

One migration pair, `20260922000001_leads_calendar.{up,down}.sql`. `btree_gist`
goes in as `create extension if not exists btree_gist` at the top; the down
migration leaves it installed, same reasoning as `vector`.

Notes on specific tables:

- **`messages` append-only.** The brief says never UPDATE or DELETE. I will
  enforce it with a rule/trigger rather than leave it as a convention, same
  philosophy as the existing guardrails: `create trigger ... before update or
  delete on messages ... raise exception`.
- **`leads`, at most one open lead per conversation.** Partial unique index:
  `create unique index on leads (conversation_id) where status in ('new','sent')`.
  Which statuses count as "open" is question 3 below.
- **`appointments`.** EXCLUDE constraint:
  `exclude using gist (resource_id with =, slot with &&) where (status = 'confirmed')`.
  That is the real anti-double-booking mechanism; application code only reports
  the error it raises.
- **`outbox`.** `idempotency_key text unique` does the dedupe. The worker claims
  with `for update skip locked`.
- **Ids.** New rows are real events, not derived facts, so they get `uuid v7`
  (time-ordered, better index locality) rather than the v5 derivation used for
  seed data. `outbox.idempotency_key` carries the idempotency, not the id.

### Milestone 2 — transcript + summary

Transcript is pure Rust over `messages`, two renderers, timestamps converted to
`dealer_settings.timezone` with `chrono-tz`. Vehicles named in
`messages.vehicle_ids` are rendered from `vehicles` by join, never from text.

Summary is one Haiku call returning strict JSON, then validated in code:

1. drop vehicles not present in any `messages.vehicle_ids` for this conversation
2. drop any field whose `evidence` message ids do not exist in this conversation
3. run the price/number guardrail over the rendered summary text
4. on a second failure, send the lead with `summary = null`

The schema lives in `config/summary_schemas/dealership.json` and is loaded at
runtime, so a second vertical is a new file rather than a code change.

**The guardrail I have to build first** (it is the missing dependency): scan the
candidate text for currency amounts and bare numbers that look like money or
mileage, and require each one to match a value actually present in the rows the
conversation touched (`vehicles.list_price_cents`, `msrp_cents`, `mileage`, plus
the figures in any retrieved `doc_chunks`). Anything unmatched is a violation →
`guardrail_events` row + retry. This is deliberately conservative: it is cheaper
to re-ask the model than to email a customer a price the database never said.

### Milestone 3 — email

- Mailpit added to `docker-compose.yml` (1025 SMTP, 8025 UI). `.env.example`
  gets `EMAIL_TRANSPORT=smtp|http`, `SMTP_URL`, `POSTMARK_TOKEN`.
- `EmailSender` trait, `SmtpSender` (lettre) and `HttpSender` (Postmark).
- Templates with `minijinja` (runtime-loaded, so a template edit does not need a
  recompile; askama would be compile-time — happy to switch if you prefer).
- ADF with `quick-xml` writer API. No string concatenation anywhere; the escaping
  test is the point of that rule.
- Outbox rows are written in the **same transaction** as the lead/appointment
  change. The worker is a separate tokio task with exponential backoff, max 5
  attempts, then `status='failed'` with `last_error`.

### Milestone 4 — calendar

`LocalCalendar` computes free slots from `business_hours` minus
`availability_exceptions` minus confirmed `appointments`, per resource, in the
dealer timezone, with `appointment_minutes` + `buffer_minutes`. Slot ids are
opaque signed strings (resource + start + kind) so `book_appointment` cannot be
handed an arbitrary range.

`GoogleCalendarProvider` is a struct with config fields, `todo!()` bodies and a
design note in `docs/google_calendar_notes.md` covering freeBusy, events.insert,
`external_event_id`, refresh-token storage, and the 7-day refresh-token expiry
while the OAuth app is in Testing.

---

## 4. The LLM client — facts that shape the design

Rust has **no official Anthropic SDK**. The documented path for an unsupported
language is raw HTTP against `POST /v1/messages`, so `llm` will be a thin
`reqwest` client. Concretely:

- Model id: **`claude-haiku-4-5`** (200K context, $1/$5 per MTok). You asked for
  Haiku; that is the current Haiku generation.
- Headers: `x-api-key`, `anthropic-version: 2023-06-01`, `content-type`.
- **Haiku 4.5 does not support `output_config.effort`** — sending it errors.
  It also uses the older thinking shape (`{"type":"enabled","budget_tokens":N}`,
  min 1024, below `max_tokens`) rather than adaptive thinking. The summary call
  does not need thinking at all.
- **Strict tool use**: `"strict": true` is a top-level field on each tool
  definition, alongside `name`/`description`/`input_schema` — not on
  `tool_choice`. It requires `additionalProperties: false` and `required` in the
  schema, and guarantees `tool_use.input` validates. All four tools get it.
- **Strict JSON for the summary**: `output_config: {format: {...}}` is the
  canonical parameter. The older `output_format` is deprecated. I will verify
  against the live API that Haiku 4.5 accepts `output_config.format` before
  relying on it; the fallback is a single strict tool the model must call, which
  gives the same schema guarantee through a different door.
- **Parallel tool use is on by default.** One assistant message may contain
  several `tool_use` blocks; every `tool_result` must go back in a **single**
  user message or the model quietly stops making parallel calls.
- Tool inputs are parsed with `serde_json`, never string-matched — escaping
  varies by model.
- `stop_reason` is checked before reading content; `tool_use` drives the loop.

`ANTHROPIC_API_KEY` comes from `.env`, and `.env` is already gitignored.

---

## 5. Testing

Every test the brief lists, plus the harness to run them:

| Test | How |
|---|---|
| ADF golden file | render a fixed lead, compare to `tests/golden/lead.xml` |
| XML escaping | customer text with `< > & " '`, emojis, `ñ á é` |
| Parse-back | re-parse generated XML, assert every required node |
| Lead twice → one email | same `idempotency_key`, assert one `outbox` row |
| SMTP failure → retries → `failed` | a sender impl that always errors |
| Booking race | two `tokio::spawn` bookings of one slot; exactly one commits |
| DST | slots across 2026-11-01 in `America/Chicago` |
| Vehicle sold mid-flow | mark sold between `get_available_slots` and `book_appointment` |

Database tests run against the real Postgres (`sqlx::test` with a per-test
transaction), not a mock. The EXCLUDE constraint cannot be tested any other way.

---

## 6. Questions before I start

**1. The missing guardrail.** Milestone 2 says to run "the existing price/number
guardrail". There isn't one. I have sketched it above — scan for money/number
tokens, require each to match a database value from the rows this conversation
touched. Is that the behaviour you want, or did you have something narrower in
mind? This is the one that most changes what "validated" means.

**2. `appointments.lead_id` ordering.** A booking is one of the triggers that
makes a lead ready, but `appointments.lead_id` is a FK — so which row exists
first? Two options: (a) `book_appointment` opens the lead (status `new`) inside
the same transaction and the appointment references it; (b) `lead_id` becomes
nullable and is backfilled when the lead is assembled. I would take (a): it keeps
the FK non-null and there is always exactly one lead per booked appointment.

**3. Which lead statuses count as "open"?** For "at most one open lead per
conversation", I read `new` and `sent` as open, `contacted` and `closed` as
closed — so a second lead can be created after the first is worked. Confirm?

**4. Who runs the idle sweep?** The "contact info exists and idle for
`lead_idle_minutes`" trigger needs something to notice. Cheapest is to fold it
into the outbox worker's tick (it is already a running tokio task) rather than
add a scheduler. Objection?

One thing I am assuming without asking: **`messages` gets a trigger that blocks
UPDATE and DELETE.** The brief says append-only, and in this codebase that kind
of rule has consistently gone into the database rather than into a convention.
Say so if you would rather it stay advisory.

---

## 7. What was built (2026-09-22)

### Decisions taken on the questions above

| # | Question | Decision |
|---|---|---|
| 1 | The missing guardrail | Built as proposed: any money-shaped figure must match inventory or the knowledge base. **Plus** a code-enforced AI disclosure on the first reply (EN/ES text lives in `dealer_settings`). |
| 2 | `appointments.lead_id` ordering | Option (a). `book_appointment` opens or reuses the lead in the same transaction; `lead_id` stays NOT NULL. |
| 3 | Open lead statuses | `new` + `sent` open, `contacted` + `closed` closed. Unchanged. |
| 4 | Idle sweep | In the worker loop, once a minute. Also queues one follow-up email to the customer. |
| - | `messages` append-only | Enforced by trigger. |

### Additions requested along the way

- **Customer appointment confirmation** (time, place, type) as its own outbox
  kind, separate from the dealership lead email and not touching lead status.
- **Follow-up email** when a customer leaves contact details and goes quiet.
- **Resumable web chat**: the browser keeps a session id; the server maps it to
  a `customer_identities` row, so a returning visitor gets their history back.

### Things found while building that changed the design

1. **The guardrail read phone numbers as prices.** Every summary containing a
   phone would fail validation twice and ship with no summary. Phones, emails,
   UUIDs, VINs and stock numbers are now stripped before scanning.
2. **A customer's stated budget is not a dealer claim.** Checking "my budget is
   $25,000" against inventory rejects it. Summary fields are now split:
   model-authored fields are checked against the dealer's data; verbatim quotes
   are checked against the exact message they cite.
3. **The append-only trigger blocked cascades**, so a conversation could never be
   deleted. It now allows DELETE when fired from a foreign-key cascade
   (`pg_trigger_depth() > 1`); editing or deleting a single message is still
   refused.

### Known limits

- **SMS is recorded, not sent.** Consent is stored with a timestamp and the
  booking path logs it, but no SMS provider is wired (out of scope per the brief).
- **Policy search is full-text, not vector.** Query embeddings in Rust need
  fastembed-rs and a ~90MB model; Postgres FTS answers the same questions on 251
  chunks today. `match_documents()` and the stored vectors are ready for the swap
  in `tools::search_policies`.
- **Slot ids are unsigned.** `book()` re-derives everything from the database and
  the EXCLUDE constraint still applies, so the exposure is small; sign them before
  the API faces the open internet.
- **The guardrail's allowed set is the whole dealer corpus**, not only the chunks
  a conversation retrieved.
- **Google Calendar** is a stub; the design is in `docs/google_calendar_notes.md`.
