---
feature: data-pipeline
files: [crates/datagen/, crates/seed/, cars/, seed/, scripts/, catalog.json, README.template.md]
status: committed
last_reviewed: 2026-10-10
---

# Data pipeline: generate, load, verify, reset

## The source data

- `cars/<slug>/specs.json`: **the source of truth** for 20 car models (pricing, powertrains,
  dimensions, safety, warranty, strengths and weaknesses, objections, talk tracks, financing
  example). It's the only file edited by hand.
- `cars/<slug>/photos/{exterior,interior}/` plus `photo-credits.json`: free-license photos from
  Wikimedia Commons with source, author and license per file. Most are CC BY-SA, so the credit must
  travel with the picture.
- `seed/knowledge_base/automotrix_knowledge_base.txt`: dealership policies.
- `seed/dealer_config.json`: settings, salespeople, hours and exceptions.

## Generators (`crates/datagen`)

| Binary | Output | How |
|---|---|---|
| `build_docs` | `cars/*/README.md`, `catalog.json`, root `README.md` | Renders from the specs; the root README is `README.template.md` with `{{CATALOG_TABLE}}` filled in |
| `fetch_photos` | Photos and credits | Searches Commons until there are 6 exterior and 4 interior photos per car. Titles must match per-car regexes (`scripts/filters.json`), so a "Civic" search can't return a Camry. |
| `gen_inventory` | `seed/inventory.json` | A **fictional** dealer inventory of real units (VIN, stock, mileage, price, colors, dates) derived from catalog data. It uses a fixed-seed ChaCha RNG, so the output is identical on every run. VINs have valid ISO 3779 check digits. Money is written DMS-style ("$28,800") to exercise the loader's normalization. |
| `build_corpus` | `seed/corpus.ndjson` | Splits the knowledge base and per-model sales notes on headings, embeds them with `bge-base-en-v1.5` (fastembed ONNX, 768 dims, CLS pooling, L2-normalized) and tags audience, risk and disclaimer. The model (about 440 MB) is cached in `.fastembed_cache/`. |

## Loader (`crates/seed`)

**`load`** applies the migrations (embedded with `sqlx::migrate!`) and then loads four groups, each
in its own transaction:

1. Inventory (dealer, vehicles, internal costs, photos)
2. Dealer config (settings, salespeople, hours replaced wholesale, exceptions)
3. Model specs (`trim_specs`)
4. The RAG corpus (documents and chunks)

Each dataset goes through three steps:

1. COPY into an all-text temporary staging table (dropped on commit).
2. Normalize and validate in SQL, stamping a `reject_reason` on bad rows.
3. `INSERT ... SELECT` with explicit casts.

If a bulk insert trips a constraint, it retries row by row inside savepoints, and the database's
error becomes the reject reason (`insert_or_isolate`). Rejects are written to
`seed/rejects/<dataset>.csv`; nothing is dropped silently.

**It's idempotent:** ids are UUID v5 of natural keys (`uid(["vehicle", dealer, vin])`), so a second
run computes the same ids and the upserts do nothing. Console output is in Spanish.

**`verify`** counts rows against the source files, checks required columns for NULLs, and runs a
**self-retrieval test**: searching with a chunk's own vector must return that chunk first. It exits
non-zero on any failure.

**`reset_db`** runs `docker compose down -v`, then up, waits for the healthcheck, then migrate,
`load` and `verify`. Pass `--rebuild-seed` to regenerate the inventory and re-embed the corpus
first.

## Commands

```bash
cargo run -p datagen --bin build_docs
cargo run -p datagen --bin fetch_photos -- scripts/cars.json scripts/filters.json
cargo run --release -p datagen --bin gen_inventory
cargo run --release -p datagen --bin build_corpus
cargo run -p seed --bin reset_db [-- --rebuild-seed]
```

## Gotchas

- `reset_db` throws away the database volume, **including every conversation, lead and
  appointment**.
- After a reset, all seeded cars are `spec_status = 'pending'`
  ([vin-spec-enrichment.md](vin-spec-enrichment.md)).
- The Dockerfile copies `seed/` into the image and runs `load` on every container start.
