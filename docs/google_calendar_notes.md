# Google Calendar provider — design notes

Status: **not implemented.** `GoogleCalendarProvider` in
`crates/automotrix/src/calendar.rs` is a struct with config fields whose three
methods return an error. This document is what it would take to make it real.

## Why it slots in cleanly

The booking flow only ever talks to the `CalendarProvider` trait:

```rust
free_slots(settings, from, to, kind, limit) -> Vec<Slot>
book(tx, settings, slot_id, request)       -> Booking
cancel(appointment_id)
```

`LocalCalendar` implements it against `business_hours`,
`availability_exceptions`, `resources` and `appointments`. A Google-backed
provider implements the same three methods, and nothing above it changes: the
LLM tools, the lead logic and the confirmation emails are unaware of which
calendar answered.

## Availability: `freeBusy.query`

`POST https://www.googleapis.com/calendar/v3/freeBusy` with one entry in `items`
per salesperson calendar and a `timeMin`/`timeMax` window. The response lists
busy intervals per calendar.

Free slots are then **our** business hours minus Google's busy intervals — not
Google's free time on its own. A salesperson's calendar being empty at 11 PM
does not mean the dealership is open at 11 PM. So `business_hours` and
`availability_exceptions` stay the source of truth for *when the store is open*;
Google only answers *who is already busy*.

Each `resources` row gains a `google_calendar_id` column to map a salesperson to
their calendar.

## Booking: `events.insert`

`POST .../calendars/{calendarId}/events` with start, end (both with an explicit
`timeZone`, never a bare UTC string, so the event renders correctly on the
salesperson's phone), a summary like "Test drive — María Núñez, 2021 RAV4", and
the customer as an attendee only if they opted in to calendar invites.

The returned event `id` is stored in **`appointments.external_event_id`**. That
column already exists. It is what `cancel()` needs, and what a sync job would use
to notice an event deleted on the Google side.

### The double-booking problem does not go away

Google has no equivalent of the `EXCLUDE` constraint. Two concurrent bookings can
both see a slot free in `freeBusy` and both `insert`. So the order stays:

1. insert the `appointments` row in Postgres — the `EXCLUDE` constraint decides
2. only if that commits, call `events.insert`
3. store the returned id in `external_event_id`

If step 2 fails, the appointment is still real and confirmed in our database;
the event creation is retried from the outbox like an email. The database stays
the judge of double-booking, as it is today.

## OAuth and refresh tokens

Offline access (`access_type=offline`, `prompt=consent`) returns a refresh
token. It is a long-lived credential to the dealer's calendar and must be stored
as a secret, per dealer:

- a `dealer_integrations` table: `dealer_id`, `provider`, `refresh_token`
  (encrypted at rest), `scopes`, `connected_at`, `last_refreshed_at`,
  `last_error`
- access tokens are short-lived (about an hour) and are only ever held in memory

Encryption at rest is part of the security work that is out of scope for now,
but this column must not ship in plaintext.

### The 7-day trap

**While the OAuth app is in "Testing" publishing status, Google expires refresh
tokens after 7 days.** Everything works in the demo, then a week later every
booking starts failing with `invalid_grant` and there is no code change to
blame.

Before any dealer relies on this:

- move the OAuth consent screen to **In production** (for the
  `calendar.events` scope this means Google's verification review), or
- accept the 7-day expiry for internal testing only, and surface `invalid_grant`
  as a clear "reconnect your calendar" state in `dealer_integrations.last_error`
  rather than a generic booking failure.

## Scopes

`https://www.googleapis.com/auth/calendar.events` for booking, plus
`https://www.googleapis.com/auth/calendar.freebusy` for availability. Avoid the
full `calendar` scope; it is broader than needed and makes verification harder.
