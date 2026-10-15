---
feature: booking-and-calendar
files: [crates/automotrix/src/calendar.rs, crates/automotrix/src/tools.rs, crates/automotrix/src/bin/server.rs, web/index.html, docs/google_calendar_notes.md]
status: committed
last_reviewed: 2026-10-10
---

# Booking and the calendar

## What it does

It offers open appointment times (test drive, visit or call) and books them without ever
double-booking a salesperson. All three kinds share one calendar.

## How free slots are computed (`LocalCalendar::free_slots`)

For each date in the range:

1. If an `availability_exceptions` row has both times NULL, the dealership is closed all day.
2. Otherwise, exception rows with times **replace** that weekday's hours.
3. Otherwise, use `business_hours` for that weekday. Several rows per day are allowed, for split
   shifts.
4. Walk each opening window in steps of `appointment_minutes + buffer_minutes` (30 + 15 by default),
   keeping only start times in the future.
5. For each time, go through the active salespeople (`resources.kind = 'salesperson'`) by name and
   take the **first** one with no confirmed appointment overlapping. One option per time, so "3
   slots" means 3 different times.

Each slot comes back with an opaque id `"<resource uuid>.<unix seconds>"` and a local label like
"Tuesday September 22, 10:00 AM CDT".

## Time zones and DST

`to_instant()` converts a local wall-clock time in the dealer's timezone to UTC:

- A time that doesn't exist (spring forward) is **dropped**.
- An ambiguous time (fall back) takes the **earlier** instant, so it's never offered twice.
- If the stored timezone name is invalid, `DealerSettings::tz()` falls back to `America/Chicago`.

## How booking works (`LocalCalendar::book`)

1. Decode the slot id. Refuse it if the time has passed or the salesperson is inactive or belongs to
   another dealer.
2. Insert a `confirmed` appointment with `slot = [start, start + appointment_minutes)`.
3. **The database decides.** The `appointments_no_overlap` EXCLUDE constraint (`resource_id =`, slot
   overlap, only where status is `confirmed`) rejects overlaps. A violation becomes `SLOT_TAKEN`,
   which is a normal outcome under concurrency, and the caller offers fresh times.

`free_slots` is only a courtesy filter. `book` is where the truth is settled.

## The three ways to book

| Path | Used by | Notes |
|---|---|---|
| `show_booking_form` tool, then the form, then `POST /api/book` | Web chat (preferred) | The form loads times from `GET /api/slots?kind=` (14 days). The submit runs the same `save_contact_info` and `book_appointment` tools, then appends a "You're booked, {first}!" assistant message to the transcript. Name and phone are required. |
| `get_available_slots`, then `save_contact_info`, then `book_appointment` | Voice, or anyone who can't use the form | The model must read the times back exactly as written. |
| Operator calendar panel, `GET /api/calendar` | Operator only | 14 days of open times plus booked ones. Other customers' bookings show only as "taken". |

## Configuration

`dealer_settings`: `timezone`, `appointment_minutes`, `buffer_minutes`. `resources`,
`business_hours` and `availability_exceptions` come from `seed/dealer_config.json`. The demo has one
salesperson (Daniela Reyes), Mon–Fri 9:00–20:00, Sat 9:00–19:00, closed Sunday, and holiday
exceptions for Thanksgiving and Christmas.

## Google Calendar

`GoogleCalendarProvider` is a **stub**: all three methods return an error. The `CalendarProvider`
trait is the seam it would plug into. The design is in `docs/google_calendar_notes.md`.

## Tests

`dst_fall_back_keeps_the_right_offsets`, `ambiguous_local_time_resolves_to_one_instant`,
`skipped_local_time_is_dropped`, `slot_ids_round_trip`. Integration:
`two_concurrent_bookings_of_one_slot_exactly_one_wins`,
`a_car_sold_between_showing_and_booking_is_refused`, `booking_without_contact_details_is_refused`.

## Gotchas

- Slot ids are **unsigned**. A caller could craft one for any time, even outside business hours. The
  EXCLUDE constraint still prevents overlaps. Signing them with an HMAC would fix this if the API
  ever faces the open internet.
- `free_slots` runs one query per candidate time per salesperson. That's fine for one salesperson
  over 14 days, but slow with many.
- `cancel()` exists but nothing calls it yet.
