---
feature: crm-panel
files: [crates/automotrix/src/bin/server.rs, web/index.html, crates/automotrix/src/twenty.rs]
status: committed
last_reviewed: 2026-10-15
---

# The operator's "CRM" panel

## What it does

The second card in the operator's left column (it replaced the old "Lead" card on 2026-10-15). It
shows how the current chat connects to the CRM: each hop from the conversation to the dealership's
systems, whether the lead has gone through each one, and the record the CRM receives.

## How it works

`GET /api/lead?session_id=` (operator only) now returns `{ lead, crm }`. `crm` is built by
`server.rs:crm_status` from the database only. The model plays no part in it, so `crm` is returned
even when no LLM is configured or the customer hasn't written yet (`lead` is `null` then).

| Field | Source |
|---|---|
| `crm.lead` | Newest row in `leads` for the conversation: `id`, `status`, `created_at`, `sent_at` (dealer local time), or `null` |
| `crm.has_contact` | `customers.phone` or `customers.email` is set |
| `crm.idle_minutes` | `dealer_settings.lead_idle_minutes` (how long a chat must be quiet before `engine::idle_sweep` opens a lead) |
| `crm.channels[email]` | `outbox` row with key `lead:{lead_id}:v1`, sent to `lead_email` + `notify_emails` |
| `crm.channels[crm]` | `outbox` row with key `adf:{lead_id}:v1`, sent to `crm_adf_email` |
| `crm.channels[sms]` | Always `not_built` (see [known-issues.md](known-issues.md)) |
| `crm.twenty.rules` | The three hand-off conditions with `met` true/false: appointment booked, name and phone, email and email consent ([crm-sync.md](crm-sync.md)) |
| `crm.twenty.sync` | The conversation's `crm_sync` row (`reason`, `status`, `attempts`, `error`, `sent_at`), or `null` |

Each channel's `delivery.status` is the outbox status (`pending`, `sending`, `sent`, `failed`) plus
`attempts`, `last_error` and `sent_at`. It is `waiting` when no lead or no outbox row exists yet,
and `off` for the CRM channel when `crm_adf_email` is empty.

The page (`web/index.html:renderCrm`) draws a vertical pipeline: **This chat** (contact on file?) →
**Automotrix lead** (opened? status) → the three channels (email, CRM ADF, SMS) with a status pill
each → **Twenty CRM** ("not qualified yet"; while `pending`: "queued", "update queued" once it is
already in Twenty, or "queued, not connected" when `crm.twenty.connected` is false; then the status
pill, plus an **Open in Twenty ↗** link from `sync.url`), followed by the three conditions as a checklist. Below it, the **Record sent to the CRM** shows the fields that used to be the
lead sheet: name, phone, email, appointment, cars, price range, purpose, payment, trade-in, timeline
and summary. These come from the `lead` object ([lead-scoring.md](lead-scoring.md)). The header
note reads `in Twenty`, `queued for CRM`, `no lead yet`, `lead pending`, `lead new/sent`, `ADF sent` or
`ADF send failed`, first match wins.

## Rules and why

- Statuses come from `leads` and `outbox`, never from the model, the same "model never writes
  facts" rule as everywhere else.
- The Twenty node shows the `crm_sync` row that `twenty.rs` works through
  ([twenty-sync.md](twenty-sync.md)). `crm_status` adds `twenty.connected` (`TWENTY_API_KEY` set),
  `sync.external_id` (the Twenty Opportunity) and `sync.url` (a link built from `TWENTY_PUBLIC_URL`).

## Tests

None automated. Checked by hand: `/api/lead` for a fresh session returns `crm` with all channels
`waiting` and SMS `not_built`. The `sent`/`failed` paths were not exercised (there were no leads in
the database).

## Gotchas

- A lead opens only after `lead_idle_minutes` of silence with contact details, so during a live
  demo the pipeline stays at "not yet" for about 15 minutes after the customer leaves a phone number.
- `/api/lead` still makes one LLM call per refresh for the `lead` part (see known-issues).
