---
feature: lead-delivery
files: [crates/automotrix/src/engine.rs, crates/automotrix/src/adf.rs, crates/automotrix/templates/lead.html, crates/automotrix/templates/lead.txt, crates/automotrix/tests/golden/lead.xml]
status: committed
last_reviewed: 2026-10-10
---

# Lead delivery: the dealership email and ADF XML

## What it does

For every `new` lead that doesn't have a lead email queued yet, it assembles and queues:

1. **The lead email** (`lead_email`) to `dealer_settings.lead_email` plus `notify_emails`, as HTML
   and plain text.
2. **The ADF email** (`adf_email`) to `dealer_settings.crm_adf_email`, if one is set. It's ADF 1.0
   XML, both in the body and as a `lead.xml` attachment.

## How it works (`engine::deliver_pending_leads` → `deliver_one`)

Up to 10 leads per pass, oldest first:

1. Build the transcript, then the summary (best effort, see [lead-summary.md](lead-summary.md)).
   Save it with `leads::set_summary`.
2. Load the vehicles shown and the first confirmed appointment, formatted in local time
   ("test drive on Tuesday September 22, 10:00 AM America/Chicago with Daniela Reyes, 2021 Toyota
   RAV4").
3. **"At a glance" yes/no facts** for the salesperson, taken from the database and the **validated**
   summary only: booked? left a phone? left an email? gave a budget? asked about financing? has a
   trade-in? said when they'll buy?
4. Subject line: `New lead: {name}, {first car}`.
5. Car cards with label, stock, VIN, price, mileage, condition, status, photo URL and link
   (`PUBLIC_BASE_URL`).
6. Render `lead.html` (minijinja with HTML auto-escaping) and `lead.txt`.
7. **In one transaction**, queue the lead email (key `lead:{id}:v1`, carrying `lead_id`) and, if a
   CRM address is set, the ADF email (key `adf:{id}:v1`). The ADF comments are the summary text plus
   the appointment, or "See transcript in the dealership lead email" if both are empty.

The lead becomes `sent` only when the outbox actually delivers the `lead_email`
([outbox-and-email.md](outbox-and-email.md)).

## ADF XML (`adf.rs`)

- Every node is written with quick-xml's event writer, so all text is escaped. There's **no string
  concatenation**. Names like `O'Brien & Sons`, `<script>`, accents and emoji all come out
  well-formed.
- Structure: `<?adf version="1.0"?>`, then `<prospect status="new">` with `id`, `requestdate`
  (dealer timezone), `vehicle` (`interest="buy"`, status, year, make, model, trim, VIN, stock,
  odometer, asking price), customer contact (first and last name, email, phone), comments, vendor
  and provider.
- `cpo` is reported as `used`: ADF 1.0 has no "certified" status, and calling a CPO car "new" would
  misrepresent it.
- ADF has no appointment node, so the booked time goes into `<comments>`.

## Tests

`golden_document` (it compares against `tests/golden/lead.xml`; regenerate with
`cargo test -p automotrix write_golden -- --ignored` and review the diff),
`escapes_hostile_customer_text`, `parses_back_with_every_required_node`, `cpo_is_reported_as_used`,
`declaration_and_processing_instruction_are_present`. Integration:
`the_same_lead_triggered_twice_is_emailed_once`.

## Gotchas

- An error assembling one lead is logged and skipped, and that lead is retried on the next pass. A
  lead that fails every time stays `new` forever with only log lines to show for it.
