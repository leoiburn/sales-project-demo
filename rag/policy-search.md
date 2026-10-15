---
feature: policy-search
files: [crates/automotrix/src/tools.rs, migrations/20260921000001_init.up.sql, migrations/20260921000002_search.up.sql, seed/knowledge_base/automotrix_knowledge_base.txt, crates/datagen/src/bin/build_corpus.rs]
status: committed
last_reviewed: 2026-10-10
---

# Policy search (the bot's knowledge base)

## What it does

It answers questions that aren't about a specific car in stock: financing, trade-ins, taxes and fees,
warranty, returns, hours, service, plus per-model sales notes. The model must quote only what comes
back.

## The corpus

- 21 documents, 251 chunks in total, stored in `documents` and `doc_chunks`:
  - `automotrix-kb` (31 chunks): the dealership policy knowledge base, from
    `seed/knowledge_base/automotrix_knowledge_base.txt`
  - `specs-<model>` (11 chunks each, 20 models): strengths, objections and talk tracks per model
- Each chunk carries:
  - `audience`: `customer`, or `system` (the bot's own operating rules, **never** retrievable)
  - `risk`: `low`, `medium` or `high` (high = money, contract or a regulated disclosure)
  - `disclaimer`: required when risk is high (a CHECK constraint enforces it)
  - `embedding`: a 768-dimension vector from `bge-base-en-v1.5`, L2-normalized, with an HNSW cosine
    index
- Built by `datagen build_corpus` into `seed/corpus.ndjson` and loaded by `seed load`
  ([data-pipeline.md](data-pipeline.md)).

## How the search works today

`tools::search_policies` uses **Postgres full-text search**, not vectors:

```sql
where dealer_id = $1 and audience = 'customer'
  and to_tsvector('english', content) @@ websearch_to_tsquery('english', $query)
order by ts_rank(...) desc limit 3
```

Each passage comes back with its heading, text, risk, and `must_include_verbatim` (the disclaimer).
The system prompt tells the model to repeat that disclaimer word for word. With no hits, the model is
told to say it doesn't know and offer a salesperson, never to answer from general knowledge.

## The vector path (ready, not wired)

`match_documents(dealer, embedding, limit=5, min_score=0.45)` exists in SQL. It does cosine search
with a relevance floor: on this corpus, on-topic questions score about 0.60–0.67 and off-topic ones
about 0.25–0.45. Using it needs a query embedding in Rust (fastembed-rs plus a ~90 MB model), which
isn't wired yet. The filters (audience, risk, disclaimer) are the same either way.

## Rules and why

- The `audience = 'customer'` filter lives in SQL, so no code path can forget it and leak the bot's
  own instructions.
- Every number in the corpus also becomes "allowed" for the price guardrail, so the bot can quote an
  APR range the knowledge base states ([price-guardrail.md](price-guardrail.md)).

## Gotchas

- Full-text search is English-stemmed. Spanish questions only match where words overlap. The model
  writes the query as the customer's words, so Spanish policy questions may come back empty.

## Related

[known-issues.md](known-issues.md), [data-pipeline.md](data-pipeline.md)
