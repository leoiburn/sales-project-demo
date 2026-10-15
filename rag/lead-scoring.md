---
feature: lead-scoring
files: [crates/automotrix/src/bin/server.rs, web/index.html]
status: committed
last_reviewed: 2026-10-15
---

# Lead scoring (the operator's "Lead profile" panel)

## What it does

`GET /api/lead?session_id=` (operator only) returns a live lead sheet and a 0–100 **buying intent**
score with a grade, rebuilt from the conversation each time. It's separate from the lead email
summary ([lead-summary.md](lead-summary.md)), with its own prompt and schema.

## How it works

1. The model gets only Customer and Assistant lines, with a skeptical prompt: "most people who chat
   ask about everything and never buy". Asking a question is not interest. It returns a headline,
   language, price range, purpose, payment (Financing, Cash, Lease or Undecided) and payment details,
   trade-in, timeline, cars of interest, signals, a 2–3 sentence summary and a next step.
2. **Every signal must quote the customer verbatim.** The quote must be at least 3 characters and
   appear (case-insensitively) in what the customer wrote. A signal without a real quote is thrown
   away, and each signal counts once.
3. **Weights** (the `SIGNALS` table, points out of 100):

   | Signal | Points |
   |---|---|
   | negotiating | +25 |
   | urgent_timeline | +15 |
   | specific_unit, hold_or_availability | +12 each |
   | out_the_door, trade_in_details, payment_specifics, competitor_quote, wants_to_come_in | +10 each |
   | decision_maker, clear_need, budget_given | +6 each |
   | just_browsing | −15 |
   | far_timeline, shopping_everything | −10 each |
   | avoids_commitment | −8 |

4. **Facts from the database outweigh anything said:** a confirmed appointment adds +25, a phone
   number +10, or an email alone +5.
5. The score is clamped to 0–100. **Hot** is 65 or more, **Warm** 40–64, **Cold** 20–39, below that
   **Browsing**.
6. Name, phone, email and appointments come from the database. Cars of interest come from the model,
   or fall back to the cars actually shown.

The page shows it as an intent bar, "Buying signals" and "Red flags" with their quotes, the next
step. The lead sheet fields (name, phone, email, appointment, cars, price range, purpose, payment,
trade-in, timeline, summary) are shown in the CRM panel as "Record sent to the CRM"
([crm-panel.md](crm-panel.md)). The same response also carries `crm`, the delivery status.

## Gotchas

- **It costs one LLM call per refresh**, and the page refreshes the side panels after every chat
  message and booking. Every customer message in an operator browser therefore triggers two model
  calls or more.
- The weights are judgment calls. Tune them against real closed deals.
- It returns `lead: null` until the customer has written something, or if no API key is set.

## Related

[web-chat.md](web-chat.md), [server-and-security.md](server-and-security.md)
