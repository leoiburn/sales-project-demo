---
feature: twenty-sync
files: [crates/automotrix/src/twenty.rs, migrations/20261015000002_twenty_sync.up.sql, crates/automotrix/src/engine.rs, crates/automotrix/src/bin/server.rs, web/index.html, docker-compose.yml, .env.example]
status: committed
last_reviewed: 2026-10-15
---

# Twenty CRM sync (the sender)

## What it does

Writes every conversation queued in `crm_sync` into Twenty CRM. The customer becomes a **Person**
and the conversation becomes an **Opportunity** whose point of contact is that Person. When the
customer, a lead or an appointment changes later, it updates the same two records. *When* a
conversation is queued is decided by `engine::crm_sweep` ([crm-sync.md](crm-sync.md)). This file
covers *how* it gets to Twenty.

Twenty runs beside the demo as its own Docker project in `~/twenty-crm`
(http://localhost:3000, its own Postgres and Redis). The admin login is in
`~/twenty-crm/ADMIN_LOGIN.txt`.

## How it works

1. **Worker.** `engine::spawn_worker` calls `twenty::tick` every 5 s, right after `crm_sweep`.
   `tick` does nothing unless `TWENTY_API_KEY` is set (`twenty::Config::from_env`).
2. **Claim** (`twenty.rs:claim`). Up to 10 rows, `FOR UPDATE SKIP LOCKED`, set to `sending` with
   `claimed_at = now()`. A row is due when it is:
   - `pending` and `next_attempt_at <= now()` (new, or a retry whose wait is over)
   - `sending` with `claimed_at` older than 5 minutes (its worker died mid-send)
   - `sent`, and something changed after `sent_at`: `customers.updated_at`, a lead's
     `coalesce(sent_at, created_at)`, or an appointment's `created_at`. This is the **resync
     rule**. `attempts` restarts at 1 for a resync.
   Conversations with `channel = 'cli'` are never claimed. The CLI and the integration tests use
   that channel, so test customers never reach the real CRM.
3. **Load** (`twenty.rs:load`). Re-reads every fact from the database: the customer row, the
   conversation channel, the latest `confirmed` appointment (with resource, dealer timezone, and
   car + list price when a car was picked), and the newest validated lead summary rendered by
   `summary::to_text`. The `crm_sync` row carries no profile data, so the model never supplies
   facts to the CRM.
4. **Ensure fields** (`Client::ensure_schema`, once per process). Reads `/rest/metadata/objects`
   and creates any missing custom field with `POST /rest/metadata/fields`. A fresh Twenty workspace
   needs no manual setup.
5. **Upsert** (`twenty.rs:push`). It finds the Person with `GET /rest/people?filter=automotrixCustomerId[eq]:"<id>"`
   and `PATCH`es it, or `POST`s a new one. Then it does the same for the Opportunity, keyed by
   `automotrixConversationId`. Because records are found by the demo's own ids, a crash after
   creating in Twenty but before marking `sent` never makes a duplicate.
6. **Finish** (`twenty.rs:finish`).
   - On success: `sent`, `sent_at`, `external_id` (the Opportunity id) and `external_person_id`.
   - On failure: back to `pending` with `last_error`, waiting 2, 4, 8, 16, then 30 minutes. After
     8 attempts (`MAX_ATTEMPTS`) the row is parked as `failed`.

### What is written

| Twenty field | Source |
|---|---|
| Person `name`, `emails.primaryEmail`, `phones.primaryPhoneNumber` | `customers.first_name/last_name/email/phone` (E.164; Twenty splits off the country code itself) |
| Person `automotrixCustomerId`, `smsOptIn`, `emailOptIn`, `preferredLanguage` | `customers.id`, `sms_opt_in`, `email_opt_in`, `preferred_language` |
| Opportunity `name` | "Name · car" (falls back to email/phone, then "Chat visitor"; car falls back to "<channel> chat") |
| Opportunity `pointOfContact` | The Person above |
| Opportunity `stage` | `MEETING` with a confirmed appointment, else `NEW` (see rules) |
| Opportunity `amount` | The booked car's `vehicles.list_price_cents` (USD) |
| Opportunity `automotrixConversationId`, `automotrixReason`, `chatChannel` | `conversations.id`, `crm_sync.reason` in words, `conversations.channel` |
| Opportunity `appointmentAt`, `appointmentDetails`, `vehicleOfInterest` | Latest confirmed appointment: start time, "test drive: Fri Nov 20, 10:00 AM (America/Chicago), with <resource>, <car>", the car |
| Opportunity `leadSummary` | Newest `leads.summary`, via `summary::to_text` |

### Operator panel

`server.rs:crm_status` adds `crm.twenty.connected` (sender on?), `sync.external_id` and `sync.url`
(`twenty::opportunity_url`, built from `TWENTY_PUBLIC_URL`). `web/index.html:renderCrm` shows
"queued" / "update queued" / "queued, not connected" for a `pending` row, the status pill
otherwise, and an **Open in Twenty ↗** link once the Opportunity exists.

## Rules and why

- **Only known fields are sent.** A null in the demo is left out of the body, so a sync never
  blanks something a salesperson typed into Twenty.
- **The stage is the salesperson's.** It is set on create. Afterwards it only moves from `NEW` to
  `MEETING` when a booking appears. Any other stage set in Twenty is never overwritten.
- **Money comes from inventory.** `amount` is the car's list price, never a figure from the chat.
- **The API key gets the Admin role.** This Twenty version only lets the Admin role be assigned to
  API keys, and creating custom fields needs it anyway.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `TWENTY_API_KEY` | empty = sender off | Key "automotrix-sync", created in Twenty (Settings → APIs & Webhooks). Lives in `.env` |
| `TWENTY_URL` | `http://host.docker.internal:3000` in Docker | How the app container reaches Twenty |
| `TWENTY_PUBLIC_URL` | `http://localhost:3000` | How a browser reaches Twenty, for the panel link |

## Tests

- Unit tests (`twenty.rs`): `unknown_fields_are_left_out_not_blanked`,
  `a_booking_sets_meeting_stage_price_and_details`, `a_stage_moved_by_the_salesperson_is_kept`,
  `no_name_falls_back_to_contact_then_visitor`. **These haven't been run yet:** there's no cargo on
  the host. The code compiled in the Docker build.
- Checked by hand on 2026-10-15 against the real Twenty, with a synthetic `web` customer (name and
  phone):
  1. It was queued, and the custom fields were created.
  2. The Person and an Opportunity (`NEW`) were created within one tick.
  3. Booking a test drive then updated the *same* Opportunity to `MEETING`, with the price, time
     and car.
  4. There was exactly one Person and one Opportunity, and no resync loop.
  5. The test data was deleted on both sides.

## Gotchas

- `save_contact_info` bumps `customers.updated_at` on every call, even when nothing changed. Each
  call re-sends the records: harmless (idempotent), but extra API calls.
- **Cancellations aren't synced.** `appointments` has no `updated_at`, so a cancelled booking
  doesn't trigger a resync. See [known-issues.md](known-issues.md).
- **Deleted records come back.** If a salesperson deletes the Person or Opportunity in Twenty, the
  next resync recreates it: the filter doesn't see soft-deleted records.
- **Sample data.** The Twenty workspace still contains Twenty's own sample companies and people.
  They are unrelated to the demo.

## Related

[crm-sync.md](crm-sync.md), [crm-panel.md](crm-panel.md), [lead-summary.md](lead-summary.md),
[running.md](running.md)
