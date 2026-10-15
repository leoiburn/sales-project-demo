---
feature: leads
files: [crates/automotrix/src/leads.rs, crates/automotrix/src/engine.rs, crates/automotrix/src/tools.rs]
status: committed
last_reviewed: 2026-10-10
---

# Leads: when they're created, handoff, idle sweep

## What it does

A **lead** is the product of the whole system: a conversation the dealership should follow up on.
Leads are created **by code, never by the model**.

## The three triggers

| Trigger | Where | What happens |
|---|---|---|
| A booking succeeds | `book_appointment` tool | The lead is opened in the same transaction as the appointment |
| The customer asks for a person | `request_human` tool | Conversation goes to `handoff` with `bot_paused`, lead opened, `handoff_email` queued to staff |
| Contact details on file, then quiet | `idle_sweep` in the worker | Runs about every 60 s (every 12th tick of the 5 s worker loop) |

**The idle sweep** finds `open` conversations whose `last_message_at` is older than
`dealer_settings.lead_idle_minutes` (15 by default), whose customer has a phone **or** email, and
which have no open lead. For each, it opens a lead. If the customer has an email, it also queues one
**follow-up email to the customer** ("Still looking, {name}?" or "¿Seguimos, {name}?", key
`followup:{conversation}:v1`) inviting them back to the chat, which still has their history.

## The lead lifecycle

`new` → `sent` → `contacted` → `closed`

- `new`: exists, nothing emailed yet.
- `sent`: the lead email really left. `sent_at` is set by the outbox worker after a successful send.
- `contacted` and `closed` are for humans. Nothing in the code sets them yet.

## Rules and why

- **At most one open lead (`new` or `sent`) per conversation.** A partial unique index enforces it,
  and `leads::open_or_reuse` returns the existing one instead of racing the index. Once a lead is
  `contacted`, the same conversation can produce a fresh lead later: a buyer returning weeks later is
  a real new lead.
- **Leads before appointments.** Booking opens the lead first, so `appointments.lead_id` can be NOT
  NULL and orphan appointments can't exist.
- **Handoff silences the bot.** With `bot_paused`, `engine::turn` stores customer messages but
  doesn't answer. The web page shows "A salesperson has been notified".

## What happens next

The worker turns every `new` lead into its emails. See [lead-delivery.md](lead-delivery.md) and
[lead-summary.md](lead-summary.md).

## Gotchas

- Nothing un-pauses a conversation or moves a lead to `contacted` or `closed`. There's no staff UI
  yet.
- `leads::has_open_lead` is defined but unused.

## Related

[outbox-and-email.md](outbox-and-email.md), [lead-scoring.md](lead-scoring.md)
