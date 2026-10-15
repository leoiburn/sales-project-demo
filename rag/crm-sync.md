---
feature: crm-sync
files: [migrations/20261015000001_crm_sync.up.sql, crates/automotrix/src/engine.rs, crates/automotrix/src/tools.rs, crates/automotrix/src/db.rs, crates/automotrix/src/bin/server.rs]
status: committed
last_reviewed: 2026-10-15
---

# CRM hand-off trigger (`crm_sync`) and email consent

## What it does

Decides **when** a conversation's lead profile goes from the system to the CRM (Twenty) and queues
it. The user's rule: send it as soon as **any one** of these is true:

| Reason (`crm_sync.reason`) | Condition (all database facts) |
|---|---|
| `appointment` | A `confirmed` appointment exists for a lead of this conversation |
| `name_phone` | `customers.phone` is set and a first or last name is set |
| `email_consent` | `customers.email` is set and `customers.email_opt_in` is true |

This is earlier than the lead email ([lead-delivery.md](lead-delivery.md)), which waits until the
chat has been quiet for `lead_idle_minutes`. Actually writing to Twenty is a separate job, the
sender in `twenty.rs`; see [twenty-sync.md](twenty-sync.md).

## How it works

- `engine::crm_sweep` runs on every worker tick (every 5 s, in `spawn_worker`, after enrichment). It
  is one `insert … select … on conflict (conversation_id) do nothing`. It finds conversations that
  meet any condition and have no `crm_sync` row yet, and inserts one with the first matching reason,
  in the order of the table above.
- `crm_sync` (migration `20261015000001_crm_sync`): `id`, `dealer_id`, `conversation_id` (**unique**:
  one hand-off per conversation), `customer_id`, `reason`, `status` (`pending` → `sending` →
  `sent`/`failed`), `attempts`, `last_error`, `external_id` (the record id in the CRM), `created_at`,
  `sent_at`. Checks: reason and status enums, and `sent` requires `sent_at`. Migration
  `20261015000002_twenty_sync` adds `next_attempt_at`, `claimed_at` and `external_person_id` for the
  sender, and replaces the `pending` index with `crm_sync_due_idx` on `pending`/`sending` rows.
- The sender (`twenty::tick`, [twenty-sync.md](twenty-sync.md)) claims due rows, re-reads the customer, lead and appointments from
  the database, and writes them to Twenty. The row carries no profile data, so the model never
  supplies facts to the CRM.
- The CRM panel ([crm-panel.md](crm-panel.md)) shows the three conditions with ✓ and the row's
  status.

### Email consent

- New columns on `customers`: `email_opt_in` (default false), `email_opt_in_at`,
  `email_opt_in_source`. Check: `not email_opt_in or email_opt_in_at is not null`, the same rule as
  SMS (consent without a time isn't evidence).
- `save_contact_info` has a new **required, non-nullable** boolean `email_opt_in`. It's recorded only
  on `true` (timestamp + source `chat`), and once given it is never cleared by a later `false`.
- The system prompt (`engine.rs`) tells the bot: when a customer gives an email, ask once whether
  they'd like emails from the dealership, and set `email_opt_in` only on an explicit yes.
- The booking form (`/api/book`) passes `email_opt_in: false`. A booking qualifies through
  `appointment` anyway.

## Rules and why

- `email_opt_in` is a plain `boolean`, not `["boolean","null"]`: the Claude API rejects tool sets
  with more than 16 union-typed parameters, and the tools are already at 17 (see known-issues).
- A sweep, not hooks in each code path: bookings come from both the chat tool and the form, and
  contact details from both too. A query over the database catches every path at once.
- One row per conversation: later changes (for example a booking after name and phone) don't add
  rows. The sender keeps the CRM record up to date: a `sent` row is claimed again when the customer,
  a lead or an appointment changed after `sent_at` (the resync rule in [twenty-sync.md](twenty-sync.md)).

## Tests

None automated yet. Checked by hand on 2026-10-15: a form booking with name and phone produced one
`crm_sync` row (`appointment`, `pending`) within a worker tick, and `/api/lead` showed the first two
rules met. The test data was then deleted.

## Gotchas

- Without `TWENTY_API_KEY` the sender is off: rows stay `pending`, and the panel says "queued, not
  connected".
- `email_opt_in` must stay a plain boolean. The tools are at 13 of the API's 16 union-typed params
  (see [tools.md](tools.md)).
