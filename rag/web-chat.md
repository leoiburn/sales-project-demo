---
feature: web-chat
files: [web/index.html, crates/automotrix/src/bin/server.rs, crates/automotrix/src/db.rs]
status: committed
last_reviewed: 2026-10-15
---

# Web chat page

## What it does

`web/index.html` is a single static page with no framework and no build step. It holds the chat,
the car cards, the booking form, a showroom carousel and, for the operator, the side panels.

## Sessions and memory

- The browser keeps a random session id in `localStorage` and sends it with every request.
- The server only accepts ids of 8–64 characters, using `[A-Za-z0-9-]` (`valid_session`).
- `db::conversation_for_identity(dealer, "web", session_id)` maps the id to a customer and a
  conversation through `customer_identities`, creating them on first contact. Closing the tab and
  coming back resumes the same chat.
- `GET /api/history` reloads past customer and assistant messages, with their car cards, plus
  `bot_paused`.
- **New chat** makes a new session id, so a new customer and conversation.

## The chat

- **Quick chips:** "SUVs under $30,000", "Busco una pickup 4x4", "Can I return a car?", "Book a test
  drive".
- `POST /api/chat` returns `{text, vehicles, handoff, form}`. Messages must be 1–2000 characters.
  Without an API key it returns 503 with a setup message. A message blocked by Laya gets a fixed
  bilingual refusal.
- **Car cards:** label, price, mileage, condition, stock, color, drivetrain, fuel, body type, and a
  photo gallery with arrows (every photo, the primary one first). Photos are served from the repo's
  `cars/` folder at `/cars/...`.
- **Booking form:** appears when the reply includes `form`. See
  [booking-and-calendar.md](booking-and-calendar.md).
- **Handoff:** shows "A salesperson has been notified and will reach out" in both languages.

## Showroom

`GET /api/gallery` returns one entry per make and model (cheapest unit first, "from $X", a unit
count), with every available unit as a card. The page shows it as a 3D ring you can drag, or move
with the arrow keys and buttons; picking one opens its units.

## Operator view vs customer view

The server decides who's an operator ([server-and-security.md](server-and-security.md)). Everyone
else gets the page with `<body class="chat-only">`, which hides the side panels. The panels are:

- **Lead profile** ([lead-scoring.md](lead-scoring.md)) and **CRM** ([crm-panel.md](crm-panel.md))
- **Appointments** calendar ([booking-and-calendar.md](booking-and-calendar.md))

They refresh on page load, after every chat reply, and after a booking.

Each panel (the four `.card` sections: Lead profile, CRM, Appointments, Showroom) has a **full
screen** button (⛶) added to its header by the script at load. Clicking it calls the browser's
Fullscreen API on that card (`card.requestFullscreen()`) and also adds the `.max` class, which pins
the card over the whole window (`position: fixed; inset: 0`). The class is the fallback for browsers
without the API for non-video elements (iPhone Safari). In full screen the button becomes ✕; the ✕,
Esc, or the browser leaving full screen (`fullscreenchange`) all restore the card. Esc inside the
showroom's car detail closes the detail first. Full screen enlarges the showroom ring
(`scale: 1.8` on `.spin`) and the detail photo (55vh), and centres the other panels' content. The CRM panel shows where a
lead is sent (email, CRM ADF, SMS, Twenty) as a pipeline with live status; see
[crm-panel.md](crm-panel.md).

## Gotchas

- The page is copied into the `automotrix-app` image (no volume). The server re-reads
  `web/index.html` on every request, so a `docker cp` into the running container shows a change at
  once; a rebuild bakes it in.

- The `chat-only` class only hides the panels. The real protection is the server's `operator_only`
  middleware on `/api/calendar` and `/api/lead`.
- Anyone who knows or guesses a session id can read that conversation's history through
  `/api/history`. The ids are random, but there's no other authentication.

## Related

[voice-calls.md](voice-calls.md), [conversation-engine.md](conversation-engine.md)
