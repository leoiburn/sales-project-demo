---
feature: inventory-search
files: [migrations/20260921000002_search.up.sql, migrations/20261002000001_inventory_color.up.sql, crates/automotrix/src/tools.rs]
status: committed
last_reviewed: 2026-10-15
---

# Inventory search

## What it does

It answers "do you have X?" with **typed SQL filters over real rows**, never with similarity search,
so the bot can only name a car that really exists in the `vehicles` table.

## How it works

The Postgres function `search_inventory(p_dealer_id, p_body_type, p_make, p_model, p_year_min,
p_max_price_cents, p_max_mileage, p_condition, p_drivetrain, p_status='available', p_limit=20,
p_color)` does the filtering:

- Every argument is optional, and NULL means "no filter".
- `make` and `model` are case-insensitive exact matches (`ilike` without wildcards).
- `drivetrain` and `color` are substring matches. Color matches the exterior **or** the interior
  color.
- Only `available` cars are returned unless another status is passed, so a sold car can't be offered
  by accident.
- Results are ordered by price, cheapest first.
- It returns: id, stock number, VIN, a label like "2021 Toyota RAV4 XLE", condition, mileage,
  price in cents, drivetrain, exterior color, days in stock, primary photo, interior color and
  features.

The Rust tool (`tools::search_inventory`) converts dollars to cents, asks for status `available`
with a limit of 6, and formats prices with `db::money()` (cents become "$24,950"). It returns every
row's `vehicle_id`, which marks those cars as **shown** on the assistant message.

## Rules and why

- Inventory is never in the RAG corpus. Mixing them is how a bot invents a car that isn't on the lot.
- Money is always `bigint` cents. No float ever touches a price.
- The color filter was added on 2026-10-02 (commit `38565a3`). The migration drops and recreates the
  function, because its return columns changed.
- `make`, `model`, `drivetrain` and `color` are plain `string` params, where an empty string means
  "any". They aren't nullable because the Claude API refuses the whole request when all the tools
  together have more than 16 union-typed params (`["string","null"]` or `anyOf`). `opt_str` turns
  a blank into `None`, so the SQL gets `null` either way. `body_type`, `condition` and the numeric
  filters stay nullable.

## Gotchas

- The tool schema says to send color as "one plain color word". Something like "dark blue" only
  matches if that exact substring is in the color name.

## Related

[tools.md](tools.md), [database.md](database.md), [policy-search.md](policy-search.md)
