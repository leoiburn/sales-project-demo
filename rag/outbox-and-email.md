---
feature: outbox-and-email
files: [crates/automotrix/src/outbox.rs, crates/automotrix/src/email.rs, crates/automotrix/src/engine.rs]
status: committed
last_reviewed: 2026-10-10
---

# Outbox and email transports

## What it does

All outgoing mail goes through a database queue (`outbox`), so a chat turn never waits on SMTP and a
lead is never lost because the mail server was down.

## Email kinds

`lead_email`, `adf_email`, `handoff_email`, `appointment_confirmation`, `followup_email`, and
`appointment_sms` (reserved, nothing sends SMS yet).

## How it works

- **Enqueue in the same transaction** as the change that caused the email (the lead, the booking,
  the handoff). Either both exist or neither does.
- **Idempotency:** `idempotency_key` is UNIQUE, and `on conflict do nothing` makes queueing the same
  email twice a no-op. Keys look like `lead:{id}:v1`, `adf:{id}:v1`, `handoff:{conversation}:v1`,
  `appt-confirm:{appointment}:v1`, `followup:{conversation}:v1`.
- **Claiming:** `tick()` claims up to 20 due `pending` rows with `FOR UPDATE SKIP LOCKED`, so
  several workers never take the same row. It sets them to `sending` and increments `attempts`.
- **On success:** `sent` with `sent_at`. For `lead_email`, the lead is also flipped to `sent`.
- **On failure:** retried after 2^attempts minutes (2, 4, 8, 16...). After 5 attempts the row is
  parked as `failed` with `last_error`. The lead itself is kept.
- **The worker** (`engine::spawn_worker`) is one tokio task inside the server. Every 5 s it runs the
  outbox tick, lead delivery and spec enrichment, and every 12th tick the idle sweep.

## Transports (`email.rs`)

Chosen by `EMAIL_TRANSPORT`:

| Value | Transport | Notes |
|---|---|---|
| `smtp` | `SmtpSender` (lettre) to `SMTP_URL` | Docker uses Mailpit at `smtp://mailpit:1025`. Every mail shows up at http://localhost:8025 and nothing leaves the machine. |
| `http` | `HttpSender` to Postmark (`POSTMARK_TOKEN`) | For production |
| unset or anything else | `CapturingSender` | Keeps mail in memory and logs it. Also the fallback when SMTP or Postmark is misconfigured. |

`FailingSender` always fails; it exists to test retries. Attachments are base64 inside the JSON
payload, so an email survives the round trip through `outbox.payload`. The `From` address comes from
`EMAIL_FROM`.

## Tests

`builds_a_mime_message_with_attachment`, `attachments_survive_a_json_round_trip`. Integration:
`the_same_lead_triggered_twice_is_emailed_once`,
`a_dead_mail_server_retries_then_gives_up_and_keeps_the_lead`,
`a_successful_send_marks_the_lead_sent`.

## Gotchas

- If the process dies between claiming a row (`sending`) and marking it, the row stays `sending`
  forever, because the claim only picks up `pending`. See [known-issues.md](known-issues.md).
- With `CapturingSender`, mail is "sent" successfully but only exists in memory, so leads are marked
  `sent` without anyone receiving them.
