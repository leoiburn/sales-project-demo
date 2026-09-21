"""
WHAT: Chunks and embeds every RAG document, writing seed/corpus.ndjson.
WHY:  The bot answers policy and product questions from this corpus. It never
      answers inventory questions from it - those are SQL over the vehicles
      table. Two document sets go in:
        1. the Automotrix knowledge base (policy, financing, warranty, FAQ)
        2. per-model sales notes (strengths, objections, talk tracks)
HOW:  Split on section headings, embed with BAAI/bge-base-en-v1.5 (768 dims,
      cosine), and tag every chunk with the risk metadata the guardrail layer
      needs. Vectors are L2-normalized, so cosine distance in pgvector is the
      matching metric.

Run with the project venv:  /home/leoiburn/.venv/bin/python scripts/build_corpus.py
"""
import glob
import hashlib
import json
import re

from sentence_transformers import SentenceTransformer

MODEL_NAME = "BAAI/bge-base-en-v1.5"
DIM = 768
KB_PATH = "seed/knowledge_base/automotrix_knowledge_base.txt"

# Legal exposure if the bot states this wrong. 'high' = money, contract or a
# regulated disclosure (TILA, Texas DTPA, Magnuson-Moss, FTC Used Car Rule);
# those chunks may never be answered without their disclaimer attached.
KB_RISK = {1: "low", 2: "low", 3: "medium", 8: "medium", 14: "medium",
           4: "high", 5: "high", 6: "high", 7: "high", 9: "high",
           10: "high", 11: "high", 12: "high", 13: "high", 15: "high"}

KB_DISCLAIMER = {
    4: "Estimate only, not an offer of credit. All rates and approvals are "
       "subject to lender review. Confirm with a finance associate.",
    5: "Tax and fee amounts are estimates and vary by county and by date. "
       "Confirm the final figure with a sales associate.",
    6: "Trade values require an in-person appraisal. No figure quoted here is "
       "an offer.",
    7: "Deposit terms are set by the signed deposit agreement, not by this chat.",
    9: "Coverage depends on the specific vehicle and the FTC Buyers Guide "
       "posted on its window. Verify before purchase.",
    10: "Texas has no cooling-off period. Exchange terms are set by the signed "
        "agreement. Confirm eligibility with a manager.",
    11: "Optional products. Terms, cancellation and refunds are governed by the "
        "product contract.",
    12: "General warranty summary only, not a legal opinion or a coverage "
        "determination. Coverage follows the VIN. For Lemon Law questions "
        "contact the customer relations manager at (210) 555-0142.",
    13: "Service prices are estimates and exclude tax. A written estimate is "
        "provided before any work begins.",
    15: "Summary answers only. For exact figures talk to an associate.",
}

SPEC_DISCLAIMER = (
    "Model-level information, not a specific vehicle. Equipment, price and "
    "availability vary by unit - confirm against the stock number in inventory."
)
PAYMENT_DISCLAIMER = (
    "Illustrative payment example only. Not an offer of credit and not a quote. "
    "Actual terms depend on lender approval, taxes and fees."
)


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def chunk_kb(raw):
    """Split on SECTION headings, then on numbered subsections (4.1, 4.2...)."""
    raw = raw.split("END OF DOCUMENT")[0]
    parts = re.split(r"\n-+\nSECTION ", raw)
    sections = [("SECTION " + p).strip() for p in parts[1:]]

    out = []
    for sec in sections:
        title = sec.split("\n")[0]
        subs = re.split(r"\n(?=\d+\.\d+ )", sec)
        pieces = [sec] if (len(sec) < 1500 or len(subs) == 1) else \
            [subs[0]] + [title + "\n" + s for s in subs[1:]]
        out += [p for p in pieces if len(p.strip()) > 100]

    chunks = []
    for i, c in enumerate(out):
        num = int(re.search(r"SECTION (\d+)", c).group(1))
        sub = re.search(r"^(\d+\.\d+)", "\n".join(c.split("\n")[1:3]), re.M)
        chunks.append({
            "chunk_index": i,
            "heading": c.split("\n")[0].strip() + (f" / {sub.group(1)}" if sub else ""),
            "content": c,
            # Section 16 is the bot's own operating rules. It belongs in the
            # system prompt, never in a customer-facing answer.
            "audience": "system" if num == 16 else "customer",
            "risk": KB_RISK.get(num, "low"),
            "disclaimer": KB_DISCLAIMER.get(num),
            "metadata": {"section": num, "subsection": sub.group(1) if sub else None},
        })
    return chunks


# Per-model sales notes. Each key becomes one chunk; the value says how to
# render it and how risky it is to get wrong.
SPEC_PARTS = [
    ("strengths",        "Strengths",            "low"),
    ("weaknesses",       "Weaknesses",           "low"),
    ("ideal_buyer",      "Ideal buyer",          "low"),
    ("use_cases",        "Use cases",            "low"),
    ("competitors",      "Cross-shopped against", "low"),
    ("sales_objections", "Objection handling",   "medium"),
    ("talk_tracks",      "Test drive and walkaround", "low"),
    ("ownership",        "Ownership costs",      "medium"),
    ("warranty",         "Warranty",             "high"),
    ("safety",           "Safety",               "medium"),
    ("financing_example", "Financing example",   "high"),
]


def render(value):
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        if value and isinstance(value[0], dict):
            return "\n".join(
                "- " + "; ".join(f"{k}: {v}" for k, v in item.items())
                for item in value)
        return "\n".join(f"- {v}" for v in value)
    if isinstance(value, dict):
        return "\n".join(f"- {k.replace('_', ' ')}: {render(v)}"
                         if not isinstance(v, (list, dict))
                         else f"- {k.replace('_', ' ')}:\n" + render(v)
                         for k, v in value.items())
    return str(value)


def chunk_specs(spec):
    label = f"{spec['model_year']} {spec['make']} {spec['model']}"
    chunks = []
    for key, heading, risk in SPEC_PARTS:
        if key not in spec:
            continue
        body = render(spec[key]).strip()
        if len(body) < 40:
            continue
        disclaimer = PAYMENT_DISCLAIMER if key == "financing_example" else \
            (SPEC_DISCLAIMER if risk == "high" else None)
        chunks.append({
            "chunk_index": len(chunks),
            "heading": f"{label} - {heading}",
            "content": f"{label} - {heading}\n{body}",
            "audience": "customer",
            "risk": risk,
            "disclaimer": disclaimer,
            "metadata": {"catalog_id": spec["id"], "make": spec["make"],
                         "model": spec["model"], "model_year": spec["model_year"],
                         "part": key},
        })
    return chunks


def main():
    model = SentenceTransformer(MODEL_NAME)
    tok = model.tokenizer
    docs = []

    kb_raw = open(KB_PATH).read()
    docs.append({
        "doc_key": "automotrix-kb",
        "title": "Automotrix - Customer Knowledge Base",
        "kind": "policy",
        "source_path": KB_PATH,
        "body": kb_raw,
        "chunks": chunk_kb(kb_raw),
    })

    for path in sorted(glob.glob("cars/*/specs.json")):
        spec = json.load(open(path))
        docs.append({
            "doc_key": f"specs-{spec['id']}",
            "title": f"{spec['model_year']} {spec['make']} {spec['model']} - sales notes",
            "kind": "specs",
            "source_path": path,
            "body": json.dumps(spec, indent=1),
            "chunks": chunk_specs(spec),
        })

    # embed everything in one pass so the batch is efficient
    flat = [c for d in docs for c in d["chunks"]]
    texts = [c["content"] for c in flat]
    over = [(i, len(tok.encode(t))) for i, t in enumerate(texts)
            if len(tok.encode(t)) > model.max_seq_length]
    for i, n in over:
        print(f"AVISO: chunk {i} tiene {n} tokens, se truncara a "
              f"{model.max_seq_length}: {flat[i]['heading'][:60]}")

    vecs = model.encode(texts, normalize_embeddings=True, batch_size=32)
    assert vecs.shape == (len(flat), DIM), vecs.shape

    for c, v in zip(flat, vecs):
        c["embedding"] = [round(float(x), 6) for x in v]
        c["embedding_model"] = MODEL_NAME
        c["token_count"] = len(tok.encode(c["content"]))
        c["content_hash"] = sha(c["content"])

    for d in docs:
        d["content_hash"] = sha(d["body"])

    with open("seed/corpus.ndjson", "w") as f:
        for d in docs:
            f.write(json.dumps(d, ensure_ascii=False) + "\n")

    from collections import Counter
    risks = Counter(c["risk"] for c in flat if c["audience"] == "customer")
    print(f"documentos: {len(docs)}  chunks: {len(flat)}  dim: {DIM}")
    print("riesgo (customer):", dict(risks),
          "| system:", sum(1 for c in flat if c["audience"] == "system"))
    print("truncados:", len(over))
    hi = [c for c in flat if c["risk"] == "high" and not c["disclaimer"]]
    assert not hi, f"high-risk sin disclaimer: {[c['heading'] for c in hi]}"
    print("high-risk sin disclaimer: 0")


if __name__ == "__main__":
    main()
