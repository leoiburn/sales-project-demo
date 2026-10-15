---
feature: database
files: [migrations/, crates/automotrix/src/db.rs]
status: committed
last_reviewed: 2026-10-15
---

# Database

Postgres 17 with `pgvector` and `btree_gist`. The schema comes **only** from `migrations/`, which
the app's `load` binary or sqlx-cli applies. Nothing goes in `docker-entrypoint-initdb.d`: that only
runs on an empty volume and would hide schema drift.

## Conventions

Money is `bigint` cents; timestamps are `timestamptz`; every tenant table has a `dealer_id`;
enum-like columns are held by CHECK constraints; there's no ORM (plain sqlx queries in `db.rs`).

## Tables

| Group | Tables |
|---|---|
| Dealer | `dealers`, `dealer_settings` (lead and CRM emails, timezone, slot length and buffer, idle minutes, AI disclosures in English and Spanish) |
| Inventory | `vehicles` (customer-safe), `vehicle_internal` (cost and sourcing, **never** shown to customers), `vehicle_photos` (with CC credits), `trim_specs` (per-model jsonb) |
| Knowledge base | `documents`, `doc_chunks` (vector(768), audience, risk, disclaimer) |
| People and chat | `customers`, `customer_identities` (channel plus external id, e.g. a web session), `conversations`, `messages` |
| Sales | `leads`, `resources` (salespeople), `business_hours`, `availability_exceptions`, `appointments` |
| Plumbing | `outbox`, `crm_sync` (CRM hand-off queue, [crm-sync.md](crm-sync.md)), `guardrail_events` |
| View | `v_inventory`, a DMS-style list sorted by days in stock |

## Rules the database enforces itself

- **No double booking:** `appointments_no_overlap` EXCLUDE on `(resource_id =, slot &&)` where the
  status is `confirmed`.
- **One open lead per conversation:** a partial unique index on `leads(conversation_id)` where the
  status is `new` or `sent`.
- **Append-only transcript:** triggers reject UPDATE and DELETE on `messages`. The one exception is a
  cascade delete from deleting a whole conversation (detected by `pg_trigger_depth() > 1`). The
  transcript is evidence, and summary citations point at message ids.
- **Every appointment has a lead:** `appointments.lead_id` is NOT NULL.
- **Every high-risk chunk carries a disclaimer:** `risk <> 'high' or disclaimer is not null`.
- **Consent has a timestamp:** `not sms_opt_in or sms_opt_in_at is not null`, and the same for
  `email_opt_in` / `email_opt_in_at`.
- **Formats:** VIN `^[A-HJ-NPR-Z0-9]{17}$`, phone E.164, a basic email regex, year 1990–2030,
  positive prices, 2–6 doors.
- **A sent lead has `sent_at`.**

## Functions

`search_inventory(...)` (see [inventory-search.md](inventory-search.md)),
`match_documents(...)` (see [policy-search.md](policy-search.md)).

## Migrations

| File | What it adds |
|---|---|
| `20260921000001_init` | Dealer, inventory, photos, specs, documents and chunks, indexes, `v_inventory` |
| `20260921000002_search` | `match_documents`, `search_inventory` |
| `20260922000001_leads_calendar` | Settings, customers, identities, conversations, messages, leads, calendar, outbox, guardrail events |
| `20261002000001_inventory_color` | `search_inventory` with color, interior and features |
| `20261002000002_vehicle_specs` | **Uncommitted.** Spec columns and `spec_status` (see [vin-spec-enrichment.md](vin-spec-enrichment.md)) |

Each one has a `.down.sql`.

## Gotchas

- `vehicle_internal` is meant to be protected by **never granting** it to the bot's database user,
  but the demo connects as one user that owns everything. The separation is in the schema, not yet
  in the grants.
