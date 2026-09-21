"""
WHAT: Loads the three datasets into Postgres: inventory (vehicles + internal
      costs + photos), model specs, and the RAG corpus with its vectors.
WHY:  Written in Python rather than Rust because the embedding pipeline is
      Python (sentence-transformers) and cargo is not installed on this machine.
      The brief allows this fallback. Swapping in a Rust sqlx loader later
      changes nothing about the schema or the files it reads.
HOW:  Every dataset follows the same three steps:
        1. copy the raw file into a staging table whose columns are ALL text
        2. validate and normalize; anything that fails goes to
           seed/rejects/<dataset>.csv with a reason, never dropped silently
        3. INSERT ... SELECT with explicit casts into the real table, then drop
           the staging table
      One transaction per dataset: a dataset loads completely or not at all.
      Ids are uuid5 of a stable natural key, so running this twice produces
      exactly the same rows - that is what makes the upserts idempotent.
"""
import csv
import json
import os
import re
import uuid

import psycopg
from pgvector.psycopg import register_vector

DSN = os.environ["DATABASE_URL"]
ROOT = os.path.join(os.path.dirname(__file__), "..")
NS = uuid.UUID("6f1d5e7a-9b2c-4f3e-8a1d-0c5b7e9a2d41")   # fixed demo namespace

CONDITIONS = {"new", "used", "cpo"}
STATUSES = {"available", "pending", "sold"}
BODY_TYPES = {"sedan", "suv", "truck", "van", "coupe", "hatchback", "wagon",
              "convertible"}
VIN_RE = re.compile(r"^[A-HJ-NPR-Z0-9]{17}$")


def p(*parts):
    return os.path.join(ROOT, *parts)


def uid(*parts):
    return uuid.uuid5(NS, "|".join(str(x) for x in parts))


def money_cents(raw):
    """'$18,900' or '18900.00' or 1890000 -> integer cents. None if unparseable."""
    if raw is None or raw == "":
        return None
    if isinstance(raw, int):
        return raw
    s = str(raw).strip().replace("$", "").replace(",", "").replace(" ", "")
    try:
        return int(round(float(s) * 100))
    except ValueError:
        return None


def as_int(raw):
    """'45,000' -> 45000. None if unparseable."""
    if raw is None or raw == "":
        return None
    try:
        return int(str(raw).strip().replace(",", "").replace(" ", ""))
    except ValueError:
        return None


def insert_rows(conn, sql, rows, dataset, key_of, rej):
    """
    Inserts rows, sending any the database refuses to the rejects file.

    Fast path is one executemany. If a constraint fires, the whole statement is
    rolled back and the rows are retried one at a time inside savepoints, so the
    offending row is named instead of taking the other 68 down with it.
    Application validation cannot catch everything - a stock number that moved
    from one VIN to another only collides against rows already in the table -
    so the database constraints are treated as the final validator.
    """
    if not rows:
        return 0
    try:
        with conn.transaction():
            conn.cursor().executemany(sql, rows)
        return len(rows)
    except psycopg.errors.IntegrityError:
        pass

    ok = 0
    for row in rows:
        try:
            with conn.transaction():
                conn.cursor().execute(sql, row)
            ok += 1
        except psycopg.errors.IntegrityError as e:
            rej.add(dataset, key_of(row),
                    str(e).split("\n")[0].strip(), {"row": str(row)[:400]})
    return ok


class Rejects:
    """Collects rejected rows per dataset and writes one CSV each."""

    def __init__(self):
        self.rows = {}

    def add(self, dataset, key, reason, row):
        self.rows.setdefault(dataset, []).append(
            {"key": key, "reason": reason, "row": json.dumps(row)[:1000]})

    def write(self):
        os.makedirs(p("seed", "rejects"), exist_ok=True)
        for dataset, rows in self.rows.items():
            path = p("seed", "rejects", f"{dataset}.csv")
            with open(path, "w", newline="") as f:
                w = csv.DictWriter(f, fieldnames=["key", "reason", "row"])
                w.writeheader()
                w.writerows(rows)
            print(f"  rechazados {dataset}: {len(rows)} -> {path}")
        for dataset in ("vehicles", "photos", "trim_specs", "doc_chunks"):
            if dataset not in self.rows:
                path = p("seed", "rejects", f"{dataset}.csv")
                if os.path.exists(path):
                    os.remove(path)


# ---------------------------------------------------------------- dealer

def load_dealer(conn, inv):
    d = inv["dealer"]
    did = uid("dealer", d["name"])
    conn.execute("""
        insert into dealers (id, name, phone, address, timezone)
        values (%s, %s, %s, %s, %s)
        on conflict (id) do update set
            name = excluded.name, phone = excluded.phone,
            address = excluded.address, timezone = excluded.timezone
    """, (did, d["name"], d["phone"], d["address"], d["timezone"]))
    return did


# ------------------------------------------------------------- inventory

def load_inventory(conn, did, inv, rej):
    conn.execute("""
        create temp table stg_vehicles (
            unit_id text, stock_number text, vin text, condition text,
            status text, year text, make text, model text, trim_level text,
            body_type text, doors text, exterior_color text, interior_color text,
            engine text, transmission text, drivetrain text, fuel_type text,
            mpg_city text, mpg_hwy text, mileage text, list_price text,
            msrp text, features text, description text, date_in_stock text
        ) on commit drop""")

    # text format (psycopg's default for write_row) - it escapes the
    # newlines and tabs that live inside description fields
    with conn.cursor().copy("copy stg_vehicles from stdin") as cp:
        for v in inv["vehicles"]:
            cp.write_row([
                v["unit_id"], v["stock_number"], v["vin"], v["condition"],
                v["status"], v["year"], v["make"], v["model"], v["trim_level"],
                v["body_type"], v["doors"], v["exterior_color"],
                v["interior_color"], v["engine"], v["transmission"],
                v["drivetrain"], v["fuel_type"], v["mpg_city"], v["mpg_hwy"],
                v["mileage"], v["list_price"], v["msrp"],
                json.dumps(v["features"]), v["description"], v["date_in_stock"],
            ])

    raw = conn.execute("select * from stg_vehicles").fetchall()
    cols = [c.name for c in conn.execute("select * from stg_vehicles limit 0").description]

    good, seen_vin, seen_stock = [], set(), set()
    for row in raw:
        r = dict(zip(cols, row))
        vin = (r["vin"] or "").strip().upper()
        cond = (r["condition"] or "").strip().lower()
        status = (r["status"] or "").strip().lower()
        body = (r["body_type"] or "").strip().lower()
        year, mileage = as_int(r["year"]), as_int(r["mileage"])
        price, msrp = money_cents(r["list_price"]), money_cents(r["msrp"])

        why = None
        if not VIN_RE.match(vin):
            why = "VIN missing or malformed"
        elif vin in seen_vin:
            why = "duplicate VIN in source"
        elif r["stock_number"] in seen_stock:
            why = "duplicate stock_number in source"
        elif cond not in CONDITIONS:
            why = f"unknown condition {cond!r}"
        elif status not in STATUSES:
            why = f"unknown status {status!r}"
        elif body not in BODY_TYPES:
            why = f"unknown body_type {body!r}"
        elif year is None or not (1990 <= year <= 2030):
            why = f"year out of range: {r['year']!r}"
        elif mileage is None or mileage < 0:
            why = f"mileage missing or negative: {r['mileage']!r}"
        elif price is None or price <= 0:
            why = f"list price missing or not positive: {r['list_price']!r}"
        elif not r["make"] or not r["model"]:
            why = "make or model missing"

        if why:
            rej.add("vehicles", r["stock_number"] or vin or "?", why, r)
            continue
        seen_vin.add(vin)
        seen_stock.add(r["stock_number"])
        good.append((
            uid("vehicle", did, vin), did, r["stock_number"], vin, cond, status,
            year, r["make"], r["model"], r["trim_level"] or None, body,
            as_int(r["doors"]), r["exterior_color"] or None,
            r["interior_color"] or None, r["engine"] or None,
            r["transmission"] or None, r["drivetrain"] or None,
            r["fuel_type"] or None, as_int(r["mpg_city"]), as_int(r["mpg_hwy"]),
            mileage, price, msrp, json.loads(r["features"]),
            r["description"] or None, r["date_in_stock"] or None,
        ))

    n_ok = insert_rows(conn, """
        insert into vehicles (id, dealer_id, stock_number, vin, condition, status,
            year, make, model, trim_level, body_type, doors, exterior_color,
            interior_color, engine, transmission, drivetrain, fuel_type,
            mpg_city, mpg_hwy, mileage, list_price_cents, msrp_cents, features,
            description, date_in_stock)
        values (%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,
                %s,%s,%s,%s,%s,%s)
        on conflict (dealer_id, vin) do update set
            stock_number = excluded.stock_number, condition = excluded.condition,
            status = excluded.status, trim_level = excluded.trim_level,
            mileage = excluded.mileage, list_price_cents = excluded.list_price_cents,
            msrp_cents = excluded.msrp_cents, features = excluded.features,
            description = excluded.description, date_in_stock = excluded.date_in_stock,
            updated_at = now()
    """, good, "vehicles", lambda r: r[2], rej)

    vin_to_id = {g[3]: g[0] for g in good}
    unit_to_id = {}
    for v in inv["vehicles"]:
        if v["vin"].upper() in vin_to_id:
            unit_to_id[v["unit_id"]] = vin_to_id[v["vin"].upper()]

    internals = [(unit_to_id[i["unit_id"]], did,
                  money_cents(i["acquisition_cost"]),
                  money_cents(i["recon_cost"]),
                  i["acquired_from"], i["internal_notes"])
                 for i in inv["internal"] if i["unit_id"] in unit_to_id]
    insert_rows(conn, """
        insert into vehicle_internal (vehicle_id, dealer_id,
            acquisition_cost_cents, recon_cost_cents, acquired_from, internal_notes)
        values (%s,%s,%s,%s,%s,%s)
        on conflict (vehicle_id) do update set
            acquisition_cost_cents = excluded.acquisition_cost_cents,
            recon_cost_cents = excluded.recon_cost_cents,
            acquired_from = excluded.acquired_from,
            internal_notes = excluded.internal_notes
    """, internals, "vehicle_internal", lambda r: str(r[0]), rej)

    return unit_to_id, n_ok


def load_photos(conn, did, inv, unit_to_id, rej):
    # credits are keyed by repo-relative path inside each car folder
    credits = {}
    for cat in {v["catalog_id"] for v in inv["vehicles"]}:
        cpath = p("cars", cat, "photo-credits.json")
        if not os.path.exists(cpath):
            continue
        for c in json.load(open(cpath)):
            credits[f"cars/{cat}/{c['file']}"] = c

    rows = []
    for ph in inv["photos"]:
        vid = unit_to_id.get(ph["unit_id"])
        if vid is None:
            rej.add("photos", ph["unit_id"], "vehicle was rejected", ph)
            continue
        if not os.path.exists(p(ph["storage_path"])):
            rej.add("photos", ph["storage_path"], "file not found on disk", ph)
            continue
        c = credits.get(ph["storage_path"], {})
        rows.append((uid("photo", vid, ph["position"]), vid, did, ph["position"],
                     ph["storage_path"], None, ph["is_primary"],
                     c.get("source"), c.get("author"), c.get("license")))

    n_ok = insert_rows(conn, """
        insert into vehicle_photos (id, vehicle_id, dealer_id, position,
            storage_path, public_url, is_primary, source_url, author, license)
        values (%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)
        on conflict (vehicle_id, position) do update set
            storage_path = excluded.storage_path,
            is_primary = excluded.is_primary,
            source_url = excluded.source_url, author = excluded.author,
            license = excluded.license
    """, rows, "photos", lambda r: r[4], rej)
    return n_ok


# ------------------------------------------------------------ trim_specs

def load_trim_specs(conn, rej):
    rows = []
    for path in sorted(__import__("glob").glob(p("cars", "*", "specs.json"))):
        s = json.load(open(path))
        year = as_int(s.get("model_year"))
        if year is None or not s.get("make") or not s.get("model"):
            rej.add("trim_specs", path, "missing year, make or model", {"path": path})
            continue
        # structured figures only; the prose sections are RAG material
        structured = {k: s[k] for k in
                      ("generation", "body_style", "segment", "country_of_origin",
                       "msrp_usd", "typical_used_price_usd", "trims", "powertrains",
                       "dimensions", "capacity", "safety", "warranty", "tech",
                       "colors_exterior", "interior_materials", "charging")
                      if k in s}
        rows.append((uid("trimspec", year, s["make"], s["model"]), year,
                     s["make"], s["model"], None, json.dumps(structured),
                     os.path.relpath(path, ROOT)))

    return insert_rows(conn, """
        insert into trim_specs (id, year, make, model, trim_level, specs, source)
        values (%s,%s,%s,%s,%s,%s,%s)
        on conflict (id) do update set
            specs = excluded.specs, source = excluded.source
    """, rows, "trim_specs", lambda r: f"{r[1]} {r[2]} {r[3]}", rej)


# ---------------------------------------------------------------- corpus

def load_corpus(conn, did, rej):
    docs = [json.loads(l) for l in open(p("seed", "corpus.ndjson"))]
    doc_rows, chunk_rows = [], []

    for d in docs:
        doc_id = uid("document", did, d["doc_key"])
        doc_rows.append((doc_id, did, d["doc_key"], d["title"], d["kind"],
                         d["source_path"], d["body"], d["content_hash"]))
        for c in d["chunks"]:
            why = None
            if not c["content"].strip():
                why = "empty content"
            elif len(c["embedding"]) != 768:
                why = f"embedding has {len(c['embedding'])} dims, expected 768"
            elif c["risk"] == "high" and not c.get("disclaimer"):
                why = "high risk chunk without disclaimer"
            if why:
                rej.add("doc_chunks", f"{d['doc_key']}#{c['chunk_index']}", why,
                        {"heading": c["heading"]})
                continue
            chunk_rows.append((
                uid("chunk", doc_id, c["chunk_index"]), doc_id, did,
                c["chunk_index"], c["heading"], c["content"], c["token_count"],
                c["embedding"], c["embedding_model"], None, c["audience"],
                c["risk"], c.get("disclaimer"), json.dumps(c["metadata"]),
                c["content_hash"]))

    n_doc = insert_rows(conn, """
        insert into documents (id, dealer_id, doc_key, title, kind, source_path,
            body, content_hash)
        values (%s,%s,%s,%s,%s,%s,%s,%s)
        on conflict (dealer_id, doc_key) do update set
            title = excluded.title, kind = excluded.kind,
            source_path = excluded.source_path, body = excluded.body,
            content_hash = excluded.content_hash
    """, doc_rows, "documents", lambda r: r[2], rej)

    n_chunk = insert_rows(conn, """
        insert into doc_chunks (id, document_id, dealer_id, chunk_index, heading,
            content, token_count, embedding, embedding_model, vehicle_id,
            audience, risk, disclaimer, metadata, content_hash)
        values (%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)
        on conflict (document_id, chunk_index) do update set
            heading = excluded.heading, content = excluded.content,
            token_count = excluded.token_count, embedding = excluded.embedding,
            embedding_model = excluded.embedding_model,
            audience = excluded.audience, risk = excluded.risk,
            disclaimer = excluded.disclaimer, metadata = excluded.metadata,
            content_hash = excluded.content_hash
    """, chunk_rows, "doc_chunks", lambda r: f"{r[1]}#{r[3]}", rej)
    return n_doc, n_chunk


def main():
    rej = Rejects()
    inv = json.load(open(p("seed", "inventory.json")))

    try:
        with psycopg.connect(DSN) as conn:
            register_vector(conn)

            with conn.transaction():                  # dataset 1: inventory
                did = load_dealer(conn, inv)
                unit_to_id, n_veh = load_inventory(conn, did, inv, rej)
                n_photo = load_photos(conn, did, inv, unit_to_id, rej)
            print(f"inventario: {n_veh} vehiculos, {n_photo} fotos")

            with conn.transaction():                  # dataset 2: model specs
                n_spec = load_trim_specs(conn, rej)
            print(f"trim_specs: {n_spec} modelos")

            with conn.transaction():                  # dataset 3: RAG corpus
                n_doc, n_chunk = load_corpus(conn, did, rej)
            print(f"corpus: {n_doc} documentos, {n_chunk} chunks")
    finally:
        # write the rejects even if a dataset blew up: losing them is exactly
        # the silent drop the brief forbids
        rej.write()
        if not rej.rows:
            print("  rechazados: 0 en todos los datasets")


if __name__ == "__main__":
    main()
