---
feature: known-issues
files: []
status: living document
last_reviewed: 2026-10-15
---

# Known issues and gaps

Remove an entry when it's fixed, and log the fix in [CHANGELOG.md](CHANGELOG.md).

## Bugs found while writing this knowledge base (2026-10-10, not yet fixed)

| Issue | Where | Effect |
|---|---|---|
| Reseeding marks every demo car `pending` for enrichment | `seed` loader + `20261002000002_vehicle_specs` | The worker queries vPIC with synthetic VINs and calls the LLM for every seeded car after `reset_db`. The fix is for the loader to set `spec_status = null` (or `done`). |
| Rows stuck in `running` or `sending` | `enrich.rs` claim, `outbox.rs` claim | A crash between claiming and finishing leaves the row in limbo forever, because claims only select `pending`. Needs a "stale after N minutes" reclaim. |
| The operator lead panel calls the LLM on every refresh | `/api/lead` + `refreshSide()` | One extra model call per chat message in an operator browser. Cache it per conversation and last message id. |

## Unfinished by design ("ponytails" in the code)

- **Vector policy search isn't wired.** `search_policies` uses full-text search, and
  `match_documents` is waiting for query embeddings in Rust.
- **The guardrail's allowed set is the whole corpus**, not only the chunks this conversation
  retrieved.
- **Slot ids are unsigned.** They should be signed (HMAC) before the API faces the internet.
- **Google Calendar provider is a stub.**
- **SMS isn't sent.** Consent is recorded, and the `appointment_sms` kind exists, but there's no
  provider.
- **Cancelled appointments aren't synced to Twenty.** `appointments` has no `updated_at`, so
  `twenty.rs:claim` can't see a cancellation. The Opportunity keeps the old appointment.
- **Records deleted in Twenty are recreated** on the next resync (`twenty.rs:push` doesn't see
  soft-deleted records).
- **Lead scoring weights and Laya thresholds** are judgment calls that need real data.
- **The voice loudness threshold** is fixed, with no calibration.

## Missing features

- No staff UI to un-pause a handed-off chat, reply as staff, or move leads to `contacted` or
  `closed`.
- `calendar.cancel()`, `leads::has_open_lead` and `db::vehicle_by_stock` are unused.
- The `vehicle_internal` protection relies on database grants that the demo doesn't set up.
- No rate limiting on any route.
- `/api/history` trusts whoever holds the session id.

## Housekeeping (from `docs/pendientes.md`, 2026-09-23)

- `cargo` isn't on `PATH`, and `clippy` isn't installed.
- The README should say that `.env` changes need `docker compose up -d app`, because `restart`
  doesn't reload them.
- Decide whether to delete test conversations `test-claude-check-0001` to `0003`.
