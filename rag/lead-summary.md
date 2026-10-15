---
feature: lead-summary
files: [crates/automotrix/src/summary.rs, config/summary_schemas/dealership.json, crates/automotrix/src/transcript.rs]
status: committed
last_reviewed: 2026-10-10
---

# Lead summary (and how it's distrusted)

## What it does

It turns the transcript into a structured JSON summary for the salesperson: name, contact, language,
cars of interest, budget, trade-in, financing, timeline, open questions and next step. It's the one
place the model writes prose that leaves the building, so code checks it before it's used.

## How it works

1. **Transcript for the model.** Every line is tagged with its message id
   (`[<uuid>] Customer: ...`), and shown cars appear as `[Shown: 2021 Toyota RAV4 XLE, Stock ...]`.
   The transcript itself is built by code from `messages` (`transcript.rs`), never by the model.
2. **One `complete_json` call** against the schema in `config/summary_schemas/dealership.json`
   (`max_tokens` 2000). Free text is written in `dealer_settings.summary_language`. Budget and
   trade-in details stay verbatim in the customer's language. Every non-null field must list its
   evidence message ids.
3. **`validate()`**, a pure function that can be tested without a model:
   1. **Cars must have been shown.** `vehicles_of_interest` entries not among the stock numbers
      actually shown in this conversation are dropped.
   2. **No evidence, no field.** A field with no cited message ids, or citing an id that isn't in
      this conversation, is nulled.
   3. **Figures:**
      - Fields the model writes itself (`financing_interest`, `timeline`, `open_questions`,
        `next_step`) go through the price guardrail against inventory and the knowledge base.
      - Fields quoting the customer (`/budget`, `/trade_in/details`) must have their numbers
        **literally present in the cited customer message**. That stops an invented "$32,000" from
        being passed off as a quote.
4. Dropped vehicles or fields are logged as a `summary_unsupported_claims` guardrail event. Figure
   failures are logged as `summary_unbacked_figure`, and the summary is retried.
5. **After two failed attempts, the lead ships without a summary**, with the transcript only. A lead
   is never lost because a summary was bad.

`to_text()` renders the validated JSON as "Label: value" lines for the email and the ADF comments.
That's done by code, not the model.

## Rules and why

- The pipeline doesn't depend on the industry: the schema and instructions are a config file
  (`config/summary_schemas/<vertical>.json`). A different vertical means a new file, not new code.

## Tests

`drops_vehicles_that_were_never_shown`, `drops_fields_without_evidence`,
`guardrail_catches_a_figure_in_the_summary`, `a_phone_number_does_not_sink_the_summary`
(regression), `a_customer_budget_is_checked_against_what_they_said`,
`an_invented_budget_is_caught_even_though_it_is_labelled_a_quote`,
`the_shipped_schema_loads_and_is_strict`.

## Related

[lead-delivery.md](lead-delivery.md), [price-guardrail.md](price-guardrail.md),
[lead-scoring.md](lead-scoring.md) (a different, operator-only summary)
