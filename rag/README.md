---
feature: index
last_reviewed: 2026-10-15
---

# Automotrix knowledge base (RAG)

This folder explains how every feature of the project works, one feature per file. It's written so
that a person or an AI agent can retrieve one file and understand that feature without reading the
code first.

**Rule:** every code change updates the matching file here and adds an entry to
[CHANGELOG.md](CHANGELOG.md). Code comments are not a substitute. See `AGENTS.md` in the repo root.

## How the files are laid out

Every file starts with frontmatter:

```yaml
feature: short-slug          # stable id, used for linking
files: [paths...]            # the code this feature lives in
status: committed | uncommitted | stub
last_reviewed: YYYY-MM-DD
```

Then the same sections, in this order: **What it does**, **How it works**, **Rules and why**,
**Configuration**, **Tests**, **Gotchas**, **Related**. Sections that have nothing to say are left out.

## Index

| File | Feature |
|---|---|
| [overview.md](overview.md) | What Automotrix is, the one core rule, and how a message flows through the system |
| [conversation-engine.md](conversation-engine.md) | One chat turn: system prompt, tool loop, disclosure, fallbacks |
| [tools.md](tools.md) | The 7 tools the model can call, and what each one validates |
| [inventory-search.md](inventory-search.md) | `search_inventory`: typed SQL filtering, including color |
| [policy-search.md](policy-search.md) | `search_policies`: the knowledge-base corpus, audience, risk and disclaimers |
| [price-guardrail.md](price-guardrail.md) | Blocking any price, rate or mileage the database can't back up |
| [booking-and-calendar.md](booking-and-calendar.md) | Free slots, booking, DST, double-booking protection, the booking form |
| [leads.md](leads.md) | When a lead is created, the handoff, the idle sweep and the follow-up email |
| [lead-summary.md](lead-summary.md) | The AI lead summary and the three checks that distrust it |
| [lead-delivery.md](lead-delivery.md) | The lead email to the dealership and the ADF XML for CRMs |
| [outbox-and-email.md](outbox-and-email.md) | The outgoing-mail queue, retries, and the SMTP/Postmark/capture transports |
| [crm-sync.md](crm-sync.md) | When a lead goes to the CRM (booked / name+phone / email+consent), the `crm_sync` queue, email consent |
| [twenty-sync.md](twenty-sync.md) | Writing queued conversations to Twenty CRM as Person + Opportunity, resync, retries. **Uncommitted** |
| [crm-panel.md](crm-panel.md) | The operator's "CRM" panel: chat → lead → email/ADF/SMS → Twenty, with live delivery status |
| [lead-scoring.md](lead-scoring.md) | The operator's "Lead profile" panel: buying signals and the Hot/Warm/Cold grade |
| [web-chat.md](web-chat.md) | The browser page: sessions, history, car cards, showroom, side panels |
| [voice-calls.md](voice-calls.md) | Talking to the bot: Whisper speech-to-text, Kokoro text-to-speech |
| [laya-screening.md](laya-screening.md) | Screening messages for jailbreaks and harm before they reach Claude |
| [server-and-security.md](server-and-security.md) | HTTPS, self-signed certificates, routes, operator-only panels |
| [vin-spec-enrichment.md](vin-spec-enrichment.md) | Auto-filling car specs from the VIN (NHTSA vPIC + LLM). **Uncommitted** |
| [database.md](database.md) | Schema, constraints, and the rules the database enforces itself |
| [data-pipeline.md](data-pipeline.md) | Generating and loading data: inventory, corpus, docs, photos, load/verify/reset |
| [running.md](running.md) | Docker services, environment variables, the CLI, everyday commands |
| [testing.md](testing.md) | What the tests cover and how to run them |
| [known-issues.md](known-issues.md) | Known gaps, unfinished pieces and bugs found while writing this |
| [CHANGELOG.md](CHANGELOG.md) | Every change, explained, newest first |

`SD_file.md` in the repo root is an older plain-Spanish explainer from 2026-09-23. Where it
disagrees with this folder, this folder is newer.
