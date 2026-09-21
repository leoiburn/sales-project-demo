# Data load report — Phase 0 (discovery)

Date: 2026-09-21
Status: **complete.** Phase 0 stopped on two blockers; both were resolved by the
product owner (generate synthetic inventory; rename the dealer to Automotrix).
Phases 1-5 then ran to a green verification. `scripts/reset_db.sh` takes the
database from nothing to loaded and verified in one command.

---

## 1. Source file inventory

The two datasets do **not** live in the same place. The inventory data is in this
repo; the RAG corpus is not in any repo — it is loose in `/home/leoiburn`.

### Dataset A — vehicle catalog (in repo)

| Path | Format | Records | Notes |
|---|---|---|---|
| `catalog.json` | JSON array | 20 | Flat index, generated from the specs files |
| `cars/<id>/specs.json` | JSON object | 20 files | Source of truth, 29 common keys |
| `cars/<id>/README.md` | Markdown | 20 files | Generated prose, same content as specs |
| `cars/<id>/photo-credits.json` | JSON array | 20 files, 7 entries each | Source URL, author, license per photo |
| `cars/<id>/photos/{exterior,interior}/*.jpg` | JPEG | 140 total (4 ext + 3 int each) | 79 MB |
| `scripts/build_docs.py` | Python | — | Regenerates READMEs + catalog.json |
| `scripts/fetch_photos.py` | Python | — | Downloads from Wikimedia Commons |
| `scripts/{cars,filters}.json` | JSON | — | Photo-fetch config, not product data |

Photo counts are uniform: every one of the 20 cars has exactly 4 exterior and 3
interior images, and every image has a credit entry. No gaps.

**3 sample records — `catalog.json`:**

```json
{"id":"audi-q5","make":"Audi","model":"Q5","model_year":2025,
 "segment":"Compact luxury SUV","msrp_usd":{"base":45400,"top":74000},
 "seating":5,"base_hp":261,"drivetrain":"quattro AWD","epa_combined":25,
 "folder":"cars/audi-q5","photos":{"exterior":[...4],"interior":[...3]}}

{"id":"honda-civic","make":"Honda","model":"Civic","model_year":2025,
 "segment":"Compact car","msrp_usd":{"base":24250,"top":45000},
 "seating":5,"base_hp":150,"drivetrain":"FWD","epa_combined":36, ...}

{"id":"porsche-911","make":"Porsche","model":"911","model_year":2025,
 "segment":"Luxury sports car","msrp_usd":{"base":120100,"top":240000},
 "seating":4,"base_hp":388,"drivetrain":"RWD","epa_combined":20, ...}
```

**`specs.json` keys** (29 present in all 20 files; `charging` is the only optional
key, present on `tesla-model-3` only, which is correct for an EV-only field):

`id, make, model, model_year, generation, body_style, segment,
country_of_origin, msrp_usd, typical_used_price_usd, trims, powertrains,
dimensions, capacity, safety, warranty, tech, colors_exterior,
interior_materials, strengths, weaknesses, ideal_buyer, use_cases, ownership,
competitors, sales_objections, talk_tracks, financing_example, data_disclaimer`

### Dataset B — RAG corpus (NOT in this repo)

| Path | Format | Records | Notes |
|---|---|---|---|
| `/home/leoiburn/dealership_knowledge_base.txt` | Plain text | 468 lines, 16 sections | Original document |
| `/home/leoiburn/chunks.json` | JSON array of strings | 31 | Chunk text only |
| `/home/leoiburn/embeddings.npy` | NumPy float32 | (31, 768) | Vectors only, ordered to match `chunks.json` |
| `/home/leoiburn/chunks.ndjson` | NDJSON | 31 | **Text + vector + metadata joined.** Use this one. |
| `/home/leoiburn/demo_rag_export.py` | Python | — | Generates `chunks.ndjson`; names the model |
| `/home/leoiburn/schema.sql` | SQL | — | An earlier standalone pgvector schema, superseded by this work |

**3 sample records — `chunks.ndjson`** (embedding truncated for readability):

```json
{"id":0,"section":1,"title":"SECTION 1: ABOUT THE DEALERSHIP","subsection":null,
 "content":"SECTION 1: ABOUT THE DEALERSHIP\n---...\nName: Lone Star Auto Group\n...",
 "audience":"customer","risk":"low","disclaimer":null,"embedding":[0.0123,...768]}

{"id":4,"section":4,"title":"SECTION 4: FINANCING","subsection":"4.2",
 "content":"SECTION 4: FINANCING\n4.2 TYPICAL APR RANGES (ESTIMATES ONLY...)...",
 "audience":"customer","risk":"high",
 "disclaimer":"Estimate only, not an offer of credit...","embedding":[...768]}

{"id":30,"section":16,"title":"SECTION 16: ASSISTANT GUIDELINES (FOR THE CHATBOT)",
 "subsection":null,"content":"...Never guarantee credit approval...",
 "audience":"system","risk":"low","disclaimer":null,"embedding":[...768]}
```

---

## 2. Embedding analysis

| Question | Answer | How it was verified |
|---|---|---|
| Storage format | `.npy` float32, plus the same vectors inlined in `chunks.ndjson` | `np.load` |
| Dimension N | **768** | `embeddings.npy.shape == (31, 768)`; every NDJSON row has exactly 768 floats |
| Dimensions identical for every vector | **Yes**, all 31 | set of per-row lengths = `{768}` |
| Model | **`BAAI/bge-base-en-v1.5`** | Hard-coded as `MODEL` in `demo_rag_export.py`; 768 dims matches |
| Similarity metric | **Cosine** | All vectors L2-normalized (min and max norm both 1.000000), so cosine and dot rank identically. Cosine is the explicit choice. |
| Chunk text stored with vector | **Yes** — `content` field, non-empty on all 31 | — |
| Source document | Yes, single document: `dealership_knowledge_base.txt` | Only one source doc exists |
| Chunk order | Yes — `id` is the 0-based ordinal | — |
| Duplicate chunks | **None** | Grouped by exact content: no collisions |
| Truncation | **None.** Largest chunk is 444 tokens against the model's 512-token window | asserted in `demo_rag_export.py` |

Extra metadata already present, carried over from earlier work in this session:
`audience` (`customer` / `system`), `risk` (`high` / `medium` / `low`), and
`disclaimer` (verbatim text to append when a high-risk chunk is retrieved).
Distribution: 30 customer-facing chunks (25 high, 3 medium, 2 low) and 1 `system`
chunk that must never be retrieved or shown to a customer.

**No re-embedding is needed or planned.** The vectors load as-is.

Query-time note: the search vector must come from the same model, and bge expects
the prefix `"Represent this sentence for searching relevant passages: "` on the
query only, never on the stored chunks.

---

## 3. Proposed mapping

### `documents` (1 row)

| Target column | Source |
|---|---|
| `title` | `"Lone Star Auto Group — Customer Knowledge Base"` |
| `kind` | `'policy'` (the document spans policy, financing, warranty, faq, hours) |
| `source_path` | `dealership_knowledge_base.txt` |
| `body` | full file text |
| `content_hash` | sha256 of body |

### `doc_chunks` (31 rows) — clean 1:1, no gaps

| Target column | Source (`chunks.ndjson`) |
|---|---|
| `chunk_index` | `id` |
| `heading` | `title` + `subsection` |
| `content` | `content` |
| `embedding` | `embedding` → `vector(768)` |
| `embedding_model` | literal `'BAAI/bge-base-en-v1.5'` |
| `token_count` | recomputed with the bge tokenizer (not stored today) |
| `vehicle_id` | always NULL — no chunk is about one specific car |
| `metadata` | `{section, subsection, audience, risk, disclaimer}` as jsonb |
| `content_hash` | sha256 of content |

### `vehicles` — **does not map.** See blockers.

| Target column | Source | Status |
|---|---|---|
| `year` | `model_year` | OK |
| `make`, `model` | same | OK |
| `body_type` | derive from `segment` / `body_style` | needs a lookup table, prose is free-form |
| `msrp_cents` | `msrp_usd.base * 100` | OK (model-level base MSRP) |
| `doors` | parse from `body_style` prose ("5-door…") | fragile |
| `mpg_city`, `mpg_hwy` | `powertrains[].epa_mpg` | OK, but per powertrain, not per vehicle |
| `drivetrain`, `fuel_type` | free-form prose (`"quattro AWD"`, `"Premium recommended"`) | needs normalization |
| `features` | derive from `tech` / `safety.standard_adas` | OK |
| `description` | `strengths` / `ideal_buyer` | OK |
| `vin` | — | **MISSING, all 20** |
| `stock_number` | — | **MISSING, all 20** |
| `mileage` | — | **MISSING, all 20** |
| `list_price_cents` | — | **MISSING, all 20** (MSRP is not a unit asking price) |
| `condition` | — | **MISSING, all 20** |
| `status` | — | **MISSING, all 20** |
| `exterior_color`, `interior_color` | `colors_exterior` is a list of options, not a unit's color | **MISSING per unit** |
| `trim_level` | `trims` is a list of names, not a unit's trim | **MISSING per unit** |
| `date_in_stock` | — | **MISSING, all 20** |

### `vehicle_photos` (140 rows) — maps cleanly

`storage_path` = `cars/<id>/photos/<kind>/<file>`, `position` = exterior 1-4 then
interior 5-7, `is_primary` = `exterior-01`. `public_url` stays NULL (not hosted).
Licensing data (`source`, `author`, `license`) from `photo-credits.json` has no
column in the target schema — see open questions.

### `trim_specs` — structured specs exist, but the grain is wrong

`dimensions`, `capacity`, `safety`, `warranty`, `tech`, `powertrains` are all
properly structured JSON and belong in `trim_specs.specs` jsonb, not in the RAG
corpus. **But** the brief's `UNIQUE (year, make, model, trim_level)` assumes specs
are per trim; here they are per **model**, with `trims` as a flat list of names and
no per-trim figures. Proposal: make `trim_level` nullable and store one row per
model (`UNIQUE (year, make, model, coalesce(trim_level,''))`).

Prose-only content that stays out of `trim_specs`: `strengths`, `weaknesses`,
`ideal_buyer`, `use_cases`, `competitors`, `sales_objections`, `talk_tracks`,
`ownership`, `financing_example`. These are sales-agent material. They are *not*
currently in the RAG corpus (which only covers the dealership policy document) —
see open questions.

---

## 4. Blockers found in Phase 0, and how they were resolved

### Blocker 1 — this repo is a model catalog, not a dealer inventory
**Resolved: generate a synthetic inventory layer, authorised explicitly.**

The brief's stop condition "any vehicle is missing VIN, year, make, model, mileage
or price" is met by **all 20 vehicles**. There is no VIN, no stock number, no
mileage, no per-unit asking price, no condition, no status, no colour, no trim and
no in-stock date anywhere in the repo, because these records describe *car models*
(what a 2025 Q5 is) rather than *units of inventory* (this specific Q5, VIN
…, 31,402 miles, $38,995, on the lot since March).

`vehicles` as specified could not be populated from the repo. Its NOT NULL and
CHECK constraints — `vin char(17)`, `mileage >= 0`, `list_price_cents > 0`,
condition and status enums — had no source data.

`scripts/gen_inventory.py` now builds that layer. Every generated field is
*derived* from real catalog data so the numbers stay internally consistent:
prices interpolate between `msrp_usd.base` and the `typical_used_price_usd`
anchors by age, then adjust for mileage and a CPO premium; trims, colours,
engines and drivetrains are drawn from the model's own lists; VINs carry a
correct ISO 3779 check digit and a real manufacturer WMI, so a VIN decoder
resolves the right make. A fixed RNG seed means the same 69 units every run,
which is what lets the loader be idempotent.

**This inventory is fictional.** It describes no real vehicle and no real person.

### Blocker 2 — the two datasets describe different dealerships
**Resolved: the dealer is now Automotrix, an independent multi-make dealer.**

`dealership_knowledge_base.txt` is **Lone Star Auto Group**, a Toyota franchise at
8420 Bandera Rd, San Antonio TX: *"We sell new Toyota vehicles, certified pre-owned
(CPO) Toyotas, and used vehicles of all makes."*

The catalog is 20 vehicles across 18 makes, including a Porsche 911, a Tesla Model
3, a Mercedes C-Class and an Audi Q5 — and exactly two Toyotas (Camry, RAV4).

`seed/knowledge_base/automotrix_knowledge_base.txt` is the rewritten document.
Every franchise claim is gone: the CPO section now describes manufacturer CPO
programs generically, the warranty section gives typical ranges across brands and
says coverage is confirmed by VIN, and service explains that factory warranty work
is routed to the brand's franchise dealer. Zero occurrences of "Lone Star",
"Toyota Certified", "TCUV" or "ToyotaCare" remain.

**This rewrite forced a re-embed of that corpus** — the brief says not to
re-embed, but the source text itself changed, so the old vectors described
sentences that no longer exist. Same model, same dimension, same metric; only the
inputs changed.

---

## 5. Decisions taken on the Phase 0 open questions

| Question | Decision |
|---|---|
| Inventory units | Generate them (option a), authorised by the product owner. 69 units across the 20 models, 2-5 per model. |
| Dealer identity | Renamed to **Automotrix**, an independent multi-make dealer. Address, phones and the fictional `555-01xx` numbers are kept. |
| Photo licensing | Added `source_url`, `author` and `license` to `vehicle_photos`, loaded from each car's `photo-credits.json`. Most images are CC BY-SA, which legally requires the credit to travel with the picture; there was nowhere to put it before. |
| Sales prose | Embedded as a second document set: one `specs` document per model, 11 chunks each (strengths, weaknesses, objections, talk tracks, warranty, financing example...). A sales bot with no sales material is not ready. Drop `chunk_specs` in `scripts/build_corpus.py` to remove it. |
| risk / audience / disclaimer | **Promoted to real columns** on `doc_chunks`, not left in `metadata` jsonb, so the guardrail is a database constraint rather than a convention the application has to remember. |

## 6. Schema decisions worth knowing

- **Money is `bigint` cents everywhere.** No float ever touches a price. The
  loader normalizes `"$28,800"` to `2880000`; `seed/inventory.json` deliberately
  stores money as the formatted string a real DMS export produces, so that
  normalization path is exercised on every run rather than being dead code.
- **`vehicle_internal` is a separate table, not columns.** Acquisition cost,
  recon cost and sourcing notes are what a customer must never see. As its own
  table, the bot's future database user simply never gets granted it — a
  guardrail the application cannot bypass by forgetting a column list.
- **`trim_specs` is per model, not per trim.** The source catalog lists trims by
  name only, with no per-trim figures. `trim_level` is nullable and uniqueness is
  an index over `coalesce(trim_level, '')`, since NULL would otherwise never
  collide with itself.
- **HNSW with `vector_cosine_ops`.** 768 dims is far below the 2000-dim pgvector
  index limit, so a plain `vector` column indexes fine and `halfvec` is not
  needed. Vectors are L2-normalized, so cosine is the matching metric.
- **The guardrails live in SQL** (`migrations/20260921000002_search.up.sql`):
  `match_documents` filters `audience = 'customer'` and applies the relevance
  floor inside the function, and `search_inventory` defaults to
  `status = 'available'` and does typed filtering only, never similarity.
  Putting either in application code means some future handler forgets it once.

## 7. Verification output

Run: `scripts/reset_db.sh` — from an empty volume to a verified database.

### 7.1 Row counts vs source

```
  [OK ] dealers: 1            - fuente 1
  [OK ] vehicles: 69          - fuente 69
  [OK ] vehicle_internal: 69  - fuente 69
  [OK ] vehicle_photos: 483   - fuente 483
  [OK ] trim_specs: 20        - fuente 20
  [OK ] documents: 21         - fuente 21
  [OK ] doc_chunks: 251       - fuente 251
```

### 7.2 Integrity

```
  [OK ] vehicles sin NULL en columnas obligatorias - 0 filas malas
  [OK ] VIN con formato valido - 0 malos
  [OK ] VIN sin duplicados - 0 repetidos
  [OK ] todos los vectores con la misma dimension - dim=768
  [OK ] un solo embedding_model - BAAI/bge-base-en-v1.5
  [OK ] ningun chunk high-risk sin disclaimer - 0 desnudos
  [OK ] exactamente una foto primaria por vehiculo
```

### 7.3 Self-retrieval

```
  [OK ] self-retrieval 20/20
```

20 random customer-facing chunks, searched with their own stored vector. Top-1
is the same chunk in every case, so text and vectors are aligned. No duplicate
`content_hash` in the corpus.

### 7.4 Sample inventory queries

`search_inventory(dealer, p_body_type => 'suv', p_max_price_cents => 2500000, p_max_mileage => 60000)`

```
  AX21-0038  2021 Mazda CX-5 2.5 Turbo Signature          45,916 mi  $22,050
  AX22-0037  2022 Mazda CX-5 S Preferred                  34,212 mi  $23,950
  AX22-0023  2022 Hyundai Tucson Hybrid SEL Convenience   33,071 mi  $24,950
```

`search_inventory(dealer, p_body_type => 'truck', p_year_min => 2019, p_drivetrain => '4WD')`

```
  AX21-0050  2021 Ram 1500 Rebel                 4WD  $35,200
  AX24-0010  2024 Chevrolet Silverado 1500 ZR2   4WD  $37,700
  AX25-0052  2025 Ram 1500 Limited               4WD  $41,550
```

### 7.5 `v_inventory`

```
  STOCK      VEHICLE                             COND   MILES     PRICE  DAYS PH STATUS
  AX25-0018  2025 Honda Civic Sport Touring Hyb  used   4,310  $24,450.00 172  7 available
  AX24-0015  2024 Ford F-150 Raptor              used  10,455  $41,200.00 171  7 pending
  AX21-0033  2021 Lexus RX RX 500h F SPORT Perf  cpo   43,715  $39,100.00 164  7 pending
  AX20-0039  2020 Mercedes-Benz C-Class C 300 4  cpo   59,250  $28,400.00 161  7 available
  AX21-0057  2021 Tesla Model 3 Performance AWD  used  44,761  $24,900.00 161  7 available
  AX23-0002  2023 Audi Q5 55 TFSI e plug-in hyb  cpo   21,672  $38,900.00 160  7 sold
  AX22-0041  2022 Mercedes-Benz C-Class AMG C 6  used  36,084  $33,550.00 158  7 available
  AX24-0051  2024 Ram 1500 Limited Longhorn      cpo   15,292  $40,500.00 154  7 available
  AX19-0046  2019 Nissan Altima SR VC-Turbo      cpo   74,671  $14,000.00 151  7 available
  AX22-0023  2022 Hyundai Tucson Hybrid SEL Con  cpo   33,071  $24,950.00 149  7 available
```

### 7.6 Guardrail tests

Beyond the brief's checklist, the guardrails were exercised directly:

- **On-topic questions carry their disclaimer.** "can I return the car after I
  sign?" returns Section 10 at 0.641 with the cooling-off disclaimer attached.
  "what APR will I get with a 620 credit score?" returns Section 4.2 at 0.610
  with the not-an-offer disclaimer.
- **Off-topic questions return nothing.** "what is the capital of France?" and
  "how do I fix a python import error?" both return 0 rows — they fall under the
  0.45 floor, which is the signal to hand off to a human.
- **The bot's own rules are unreachable.** Searching with the literal text of
  Section 16 returns 2 rows, of which 0 are `audience = 'system'`. The chunk is
  in the table; `match_documents` cannot return it.
- **Sold cars are never offered.** `search_inventory` over all 69 units returns
  0 rows whose status is not `available`.
- **No cost column exists on `vehicles`.**

### 7.7 Rejection path

Tested by corrupting five rows (malformed VIN, missing price, negative mileage,
duplicate VIN, invalid condition enum) and reloading. Result: 6 rejects written
to `seed/rejects/vehicles.csv` — the 5 caught by application validation plus one
caught by the database (`vehicles_stock_uq`, a stock number that moved to another
VIN, which only collides against rows already in the table). The load continued
with the remaining 63 vehicles, and 28 photos of rejected cars were rejected with
the reason "vehicle was rejected". Nothing was dropped silently.

This test found a real bug: the first version validated uniqueness only within
the incoming batch, so a database-level collision aborted the whole dataset *and*
lost the rejects file. `insert_rows()` now retries failed batches row by row
inside savepoints, and the rejects are written from a `finally` block.

## 8. Deliverables

| Path | What |
|---|---|
| `docker-compose.yml` | pgvector/pgvector:pg17, named volume, healthcheck, env from `.env` |
| `.env.example` | committed template; `.env` is gitignored |
| `migrations/20260921000001_init.{up,down}.sql` | the schema |
| `migrations/20260921000002_search.{up,down}.sql` | `match_documents` + `search_inventory` |
| `scripts/migrate.py` | applies migrations in order, tracks them, supports `down` |
| `scripts/gen_inventory.py` | builds the synthetic inventory |
| `scripts/build_corpus.py` | chunks and embeds both document sets |
| `scripts/load.py` | staging → validate → typed insert, with rejects |
| `scripts/verify.py` | the Phase 4 checks |
| `scripts/reset_db.sh` | nothing → verified database, one command |
| `seed/knowledge_base/automotrix_knowledge_base.txt` | rewritten policy document |
| `seed/inventory.json`, `seed/corpus.ndjson` | generated data |
| `requirements.txt` | loader and embedding dependencies |

## 9. Known limitations

- **No Rust loader.** `cargo` is not installed on this machine, so the loader is
  Python (`psycopg` + `pgvector`), which the brief allows as the fallback. The
  migrations use sqlx-cli's file naming, so `sqlx migrate run` can take over
  unchanged once cargo is available. `scripts/migrate.py` tracks state in its own
  `schema_migrations` table, not sqlx's `_sqlx_migrations`.
- **Trims are applied across model years.** Trim names come from the 2025 catalog
  but units span 2019-2025, so a 2020 Corvette can be generated with a trim that
  did not exist that year. Harmless for a demo; fix by adding per-year trim lists
  to the catalog.
- **`psycopg.Pipeline ... pipeline aborted`** prints to stderr when a batch
  insert fails and falls back to row-by-row. It is psycopg's own cleanup notice
  on the error path, not a failure.
- **Security is deliberately out of scope**, as specified. Nothing here has RLS,
  roles, auth or encryption. Before this faces anything real: a read-only role
  for the bot that is not granted `vehicle_internal`, RLS by `dealer_id` on every
  tenant table, and a rate limit in front of the API.
