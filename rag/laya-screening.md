---
feature: laya-screening
files: [crates/automotrix/src/bin/server.rs, docker-compose.yml, .env.example]
status: committed
last_reviewed: 2026-10-10
---

# Laya message screening

## What it does

It screens each customer chat message **before** it reaches Claude, using Laya: a small local
classifier running on the host as `laya.service`.

## How it works (`laya_flags`)

1. If `LAYA_URL` is unset or empty, screening is skipped entirely.
2. It posts to `{LAYA_URL}/v1/systemone` with a bearer `LAYA_API_KEY` and a 10 s timeout, asking
   three questions about the message:
   - `jailbreak` (yes/no probability): is it trying to make the AI ignore its rules?
   - `prompt_injection` (yes/no probability): are there instructions aimed at the AI rather than a
     real request?
   - `harm_severity` (a score from none through minor and serious to severe)
3. It blocks if **jailbreak > 0.5**, **prompt_injection > 0.7** or **harm ≥ 1.6**.
4. A blocked message:
   - is logged as a `laya_block` guardrail event (with reason, session id and text, and no
     conversation id);
   - gets a fixed bilingual refusal ("I can't help with that... / No puedo ayudar con eso...") with
     `blocked: true`;
   - **is not stored in the transcript** and never reaches the model.
5. **It fails open.** If Laya is down or errors, the message goes through and a warning is logged.

## Configuration

`LAYA_URL` (in Docker it defaults to `http://host.docker.internal:8765`; set it to empty to turn
screening off, which suits low-RAM machines since Laya uses about 3–5 GB), `LAYA_API_KEY`. Commit
`176d9ae` made it optional.

## Gotchas

- The thresholds were picked from a handful of test prompts (normal chat scores harm around 0.7,
  attacks 1.8 or more). Tune them with real traffic.
- Voice transcripts also go through `/api/chat`, so they're screened too.
