---
feature: tools
files: [crates/automotrix/src/tools.rs]
status: committed
last_reviewed: 2026-10-15
---

# The model's tools

## What it does

These are the only actions the model can take. **Limit:** across all tools, at most 16 params may be
union-typed (a type array like `["string","null"]`, or `anyOf`), or the API returns 400 for every
chat turn. There are 13 now. The test `union_typed_params_stay_within_the_api_limit` enforces it. For a
new optional free-text param, use a plain `string` where empty means "not given" (`opt_str` drops
blanks).

Each tool has a strict JSON schema, and Rust code
re-checks the real world (the database) when the tool is called. A tool returns an `Outcome`: JSON
text for the model, the vehicle ids it caused to be shown, `is_error`, `handoff`, and an optional
booking `form`.

## The tools

| Tool | What it does | Main checks |
|---|---|---|
| `search_inventory` | Filters available cars by body type, make, model, minimum year, maximum price (USD), maximum mileage, condition, drivetrain and color. Returns up to 6. | Typed SQL only, see [inventory-search.md](inventory-search.md). With no results it returns a note: offer to widen the search or take details. |
| `search_policies` | Searches the policy knowledge base. | Customer-audience chunks only. With no results the model must say it doesn't know. See [policy-search.md](policy-search.md). |
| `get_available_slots` | Lists up to 3 open times between two dates. | Dates must be `YYYY-MM-DD` with `date_to ≥ date_from`; the range is capped at 14 days. Returns opaque slot ids and times already formatted in local time ("read these back exactly"). |
| `book_appointment` | Books a slot returned by `get_available_slots`. | The customer must have a phone or email on file. The car is re-checked: it must still exist and still be `available`. The lead is opened in the same transaction. "Slot taken" means offer fresh times. |
| `save_contact_info` | Saves name, phone, email, language and SMS consent. | Phone is normalized to E.164 (US-centric) or rejected. Email gets a basic validity check. `coalesce` never blanks an existing value. SMS consent is recorded only on `true`, with a timestamp and source `chat`. `email_opt_in` (required plain boolean, not nullable) works the same way and feeds the CRM hand-off ([crm-sync.md](crm-sync.md)). |
| `show_booking_form` | Shows the in-chat booking form (test drive or visit). | The car only goes on the form if it's a real, available unit. The form is prefilled with the saved name, phone and email. |
| `request_human` | Hands the chat to a person. | Sets `status = 'handoff'` and `bot_paused = true`, opens or reuses the lead, and queues a `handoff_email`, all in one transaction. |

## How booking works inside `book_appointment`

1. Check that the customer is reachable and the car is still available.
2. Begin a transaction, then `leads::open_or_reuse`. The lead must exist first because
   `appointments.lead_id` is NOT NULL.
3. `LocalCalendar::book`. The database's EXCLUDE constraint makes the final call on overlaps, and a
   `SLOT_TAKEN` error rolls back.
4. If the customer gave an email, queue an `appointment_confirmation` email to them (key
   `appt-confirm:{appointment_id}:v1`).
5. If they opted in to SMS: no SMS provider exists yet, so this is only logged.
6. Commit, then return `confirmed: true` with the time to repeat exactly.

## Rules and why

- **All schemas go through `obj()`**, which sets `additionalProperties: false` and lists every
  property as required. That's what strict mode demands. Optional arguments are nullable types
  instead.
- **Optional enums use `anyOf`**: `[{type: string, enum: [...]}, {type: null}]`. The API rejects
  `"type": ["string","null"]` next to an `enum`; that bug broke the chat on 2026-09-23.
- **Re-check at call time.** A car can sell between being shown and being booked. Refusing is better
  than confirming a test drive on a sold car.

## Tests

`normalizes_phones_people_actually_type`, `rejects_addresses_that_are_not_emails`,
`every_tool_schema_satisfies_strict_mode` (it checks strict mode, `additionalProperties`, required ==
properties, and no `enum` alongside a type array). The integration tests in
[testing.md](testing.md) cover booking races, sold cars and missing contact details.

## Related

[booking-and-calendar.md](booking-and-calendar.md), [leads.md](leads.md),
[conversation-engine.md](conversation-engine.md)
