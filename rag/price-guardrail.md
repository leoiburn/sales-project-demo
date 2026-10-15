---
feature: price-guardrail
files: [crates/automotrix/src/guardrail.rs]
status: committed
last_reviewed: 2026-10-10
---

# Price guardrail

## What it does

It rejects any money-shaped figure in the bot's text that the dealer's data can't back up. Prices,
APRs, fees and mileages are what can cost real money: a quoted "3.9% APR" is a statement a customer
can act on.

## How it works

1. **Allowed set** (`allowed_for_dealer`): every list price and MSRP (written both as "24950" and
   "24950.00"), every mileage and year in the dealer's `vehicles`, plus **every number in the
   dealer's knowledge-base chunks**.
2. **Strip non-figures first** (`not_a_figure_re`): emails, phone numbers (E.164 and US formats),
   UUIDs, 17-character VINs and stock numbers like `AX21-0061` are removed before scanning. Without
   this, any reply that repeats a phone number would trip the guardrail.
3. **Scan** (`money_re`): `$12,345.67`, `$1200`, percentages like `6.5%`, comma-grouped numbers, and
   bare numbers of four or more digits.
4. **Normalize** (`normalize_number`): remove `$`, `,` and `%`, strip leading zeros and trailing
   decimal zeros. So "$24,950.00" becomes "24950" and "6.50%" becomes "6.5".
5. **Skip noise:** a bare number (no `$` or `%`) under 1000 is ignored, and so is a bare number from
   1990 to 2030 written without a comma (a model year).
6. Anything left that isn't in the allowed set becomes a `Violation {token, normalized, reason}`.

## Where it's used

- **Chat replies** ([conversation-engine.md](conversation-engine.md)): a violation logs a
  `reply_unbacked_figure` event, the model gets one rewrite naming the bad figures, and if the
  rewrite still fails a fixed safe message is sent instead.
- **Lead summaries** ([lead-summary.md](lead-summary.md)): fields the model wrote itself are checked
  against the allowed set. Fields quoting the customer are checked against the message they cite.
- **Voice:** separately, the web page never speaks prices aloud ([voice-calls.md](voice-calls.md)).

## Tests

`normalizes_equivalent_spellings`, `accepts_a_price_that_is_in_inventory`,
`rejects_an_invented_price`, `rejects_an_invented_rate`, `accepts_a_rate_the_knowledge_base_states`,
`ignores_years_and_small_counts`, `phones_emails_and_ids_are_not_figures`,
`stripping_does_not_hide_a_real_price_next_to_a_phone`, `catches_an_invented_mileage`.

## Gotchas

- The allowed set is the **whole** dealer corpus, not only the chunks retrieved in this conversation.
  The bot could quote a real figure from an unrelated section. Narrowing it would require storing the
  retrieved chunk ids per message.
- Spelled-out numbers ("twenty-five thousand") and "$25k" are matched only as "$25", not as 25,000.
