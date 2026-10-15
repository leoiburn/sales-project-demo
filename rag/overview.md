---
feature: overview
files: [crates/automotrix/src/lib.rs, Cargo.toml]
status: committed
last_reviewed: 2026-10-10
---

# Overview

## What it does

Automotrix is a demo AI sales assistant for a fictional multi-make car dealership. A customer chats
(or talks) with it in a browser, in English or Spanish. It can:

- search the dealership's real inventory and show cars as cards with photos
- answer policy questions (financing, trade-ins, warranty, returns, hours) from a knowledge base
- book test drives and visits without double-booking anyone
- collect name, phone and email when the customer offers them
- hand the chat to a human
- turn the conversation into a **lead**: an email to the dealership with a validated summary, the
  transcript and an ADF XML file for the dealership's CRM

## The one core rule

**The model never writes facts.** Claude (`claude-haiku-4-5` by default) calls tools with structured
arguments, and Rust code answers those calls from the database. Every price, car, time slot, email and
XML document is rendered from database rows. If a number reaches a customer, a SQL `SELECT` put it
there. Several layers back this up:

1. Tools are the only way to learn facts ([tools.md](tools.md)).
2. The price guardrail rejects any money-shaped figure the database can't back up
   ([price-guardrail.md](price-guardrail.md)).
3. The lead summary is checked against the transcript before it leaves the building
   ([lead-summary.md](lead-summary.md)).
4. The database enforces the rest with constraints and triggers ([database.md](database.md)).

## Architecture

Three Rust crates in one Cargo workspace:

| Crate | Role |
|---|---|
| `crates/automotrix` | The bot: engine, tools, guardrails, leads, email, calendar, HTTPS server (`server`), terminal client (`cli`) |
| `crates/seed` | Loads data into Postgres (`load`), checks it (`verify`), rebuilds everything (`reset_db`) |
| `crates/datagen` | Produces the data: synthetic inventory, the embedded corpus, car READMEs, photos |

Runtime services come from `docker-compose.yml` (see [running.md](running.md)): Postgres with
pgvector, Mailpit (a fake inbox), Whisper (speech-to-text), Kokoro (text-to-speech) and the app.
Optionally, Laya runs on the host and screens messages.

## How one message flows

1. The browser posts `{session_id, text}` to `/api/chat`.
2. The server checks the input and that the API key is set, then screens the message with Laya if
   Laya is configured ([laya-screening.md](laya-screening.md)).
3. The session id is mapped to a customer and a conversation ([web-chat.md](web-chat.md)).
4. `engine::turn` stores the message and calls Claude with the tools. It runs each requested tool
   and loops, at most 8 rounds ([conversation-engine.md](conversation-engine.md)).
5. The final text goes through the price guardrail. On the first reply, the AI disclosure is added
   in front.
6. The reply goes back with car cards, an optional booking form, and a handoff flag.
7. Separately, a background worker runs every 5 seconds. It sends queued mail, turns new leads into
   emails, enriches newly added cars, and every minute sweeps for idle conversations
   ([leads.md](leads.md), [outbox-and-email.md](outbox-and-email.md)).

## Shared helpers (`lib.rs`)

- `App` holds the database pool, LLM client, mailer, templates and summary schema, cloned per
  request.
- `repo_root()` returns `AUTOMOTRIX_ROOT` if set (it's `/app` in Docker), otherwise the path baked in
  at compile time.
- `new_id()` returns a UUID v7 (time-ordered) for event rows. Seed rows use UUID v5 instead
  (deterministic, see [data-pipeline.md](data-pipeline.md)).

## Related

[running.md](running.md), [known-issues.md](known-issues.md)
