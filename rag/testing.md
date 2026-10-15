---
feature: testing
files: [crates/automotrix/tests/integration.rs, crates/datagen/tests/catalog.rs]
status: committed
last_reviewed: 2026-10-15
---

# Testing

```bash
cargo test --workspace                 # everything (integration tests need the database up)
cargo test -p automotrix               # bot unit and integration tests
cargo test -p datagen --test catalog   # every car folder complete, every photo credited
```

Doctests are turned off for `automotrix`, because its WHAT/WHY/HOW module comments look like code
blocks to rustdoc.

## Unit tests, by module

| Module | What they prove |
|---|---|
| `guardrail` | Invented prices, rates and mileages are caught; real ones, years, phones, emails, UUIDs and VINs pass |
| `summary` | Unshown cars and uncited fields are dropped; invented figures are caught; customer quotes are checked against their message |
| `tools` | Phone and email normalization; every schema satisfies strict mode, with no `enum` beside a type array |
| `calendar` | DST fall-back offsets, ambiguous and skipped local times, slot id round trip |
| `adf` | Golden XML document, hostile text escaping, required nodes, CPO reported as used |
| `email` | MIME message with attachment, payload JSON round trip |
| `llm` | Fenced JSON parsing, tool_result serialization, unknown blocks ignored |
| `engine` | Spanish detection, merging consecutive turns |
| `twenty` | Unknown fields left out, booking sets Meeting/price/details, a salesperson's stage is kept, name fallbacks (not run yet: no cargo on the host) |
| `enrich` | vPIC field parsing and range checks, capping untrusted LLM output |

## Integration tests (`crates/automotrix/tests/integration.rs`, real Postgres)

- `two_concurrent_bookings_of_one_slot_exactly_one_wins`
- `a_car_sold_between_showing_and_booking_is_refused`
- `booking_without_contact_details_is_refused`
- `the_same_lead_triggered_twice_is_emailed_once`
- `a_dead_mail_server_retries_then_gives_up_and_keeps_the_lead`
- `a_successful_send_marks_the_lead_sent`
- `messages_cannot_be_edited_or_deleted_one_by_one`
- `save_contact_info_normalizes_and_rejects`

They create their own customers and clean up afterwards.

## Not covered

There are no tests for the HTTP routes, the web page, voice, Laya, lead scoring, or a full
model-in-the-loop conversation.
