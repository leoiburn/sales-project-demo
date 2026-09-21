"""
WHAT: Proves the loaded database is complete and that vector search works.
WHY:  A silently half-loaded RAG database still answers questions - just wrongly.
      These checks fail loudly instead.
HOW:  Compares row counts against the source files, asserts the required columns
      hold no NULLs, then runs a self-retrieval test: searching with a chunk's
      own stored vector must return that same chunk first. Anything less means
      the vectors and the text got out of alignment.
"""
import glob
import json
import os
import random

import psycopg
from pgvector.psycopg import register_vector

DSN = os.environ["DATABASE_URL"]
ROOT = os.path.join(os.path.dirname(__file__), "..")
FAIL = []


def check(label, ok, detail=""):
    print(f"  [{'OK ' if ok else 'FAIL'}] {label}{' - ' + detail if detail else ''}")
    if not ok:
        FAIL.append(label)


def main():
    inv = json.load(open(os.path.join(ROOT, "seed", "inventory.json")))
    corpus = [json.loads(l) for l in
              open(os.path.join(ROOT, "seed", "corpus.ndjson"))]
    src_chunks = sum(len(d["chunks"]) for d in corpus)
    src_specs = len(glob.glob(os.path.join(ROOT, "cars", "*", "specs.json")))

    with psycopg.connect(DSN) as conn:
        register_vector(conn)
        q = lambda s, *a: conn.execute(s, a).fetchone()

        print("\n1. CONTEO DE FILAS vs FUENTE")
        for table, expected in [
            ("dealers", 1), ("vehicles", len(inv["vehicles"])),
            ("vehicle_internal", len(inv["internal"])),
            ("vehicle_photos", len(inv["photos"])),
            ("trim_specs", src_specs), ("documents", len(corpus)),
            ("doc_chunks", src_chunks),
        ]:
            got = q(f"select count(*) from {table}")[0]
            check(f"{table}: {got}", got == expected, f"fuente {expected}")

        print("\n2. INTEGRIDAD")
        nulls = q("""select count(*) from vehicles where vin is null
                     or year is null or make is null or model is null
                     or mileage is null or list_price_cents is null""")[0]
        check("vehicles sin NULL en columnas obligatorias", nulls == 0,
              f"{nulls} filas malas")
        bad_vin = q("select count(*) from vehicles where vin !~ '^[A-HJ-NPR-Z0-9]{17}$'")[0]
        check("VIN con formato valido", bad_vin == 0, f"{bad_vin} malos")
        dup = q("""select count(*) from (select vin from vehicles
                   group by vin having count(*) > 1) t""")[0]
        check("VIN sin duplicados", dup == 0, f"{dup} repetidos")
        dims = q("select count(distinct vector_dims(embedding)) , min(vector_dims(embedding)) from doc_chunks")
        check("todos los vectores con la misma dimension", dims[0] == 1, f"dim={dims[1]}")
        models = conn.execute("select distinct embedding_model from doc_chunks").fetchall()
        check("un solo embedding_model", len(models) == 1,
              models[0][0] if models else "ninguno")
        naked = q("select count(*) from doc_chunks where risk = 'high' and disclaimer is null")[0]
        check("ningun chunk high-risk sin disclaimer", naked == 0, f"{naked} desnudos")
        prim = q("""select count(*) from (select vehicle_id from vehicle_photos
                    where is_primary group by vehicle_id having count(*) <> 1) t""")[0]
        check("exactamente una foto primaria por vehiculo", prim == 0)

        print("\n3. SELF-RETRIEVAL (20 chunks al azar, top-1 debe ser el mismo)")
        ids = [r[0] for r in conn.execute(
            "select id from doc_chunks where audience = 'customer'").fetchall()]
        sample = random.Random(7).sample(ids, min(20, len(ids)))
        hits, misses = 0, []
        for cid in sample:
            vec = conn.execute(
                "select embedding from doc_chunks where id = %s", (cid,)).fetchone()[0]
            top = conn.execute("""select id from doc_chunks
                                  where audience = 'customer'
                                  order by embedding <=> %s limit 1""",
                               (vec,)).fetchone()[0]
            if top == cid:
                hits += 1
            else:
                misses.append((cid, top))
        check(f"self-retrieval {hits}/{len(sample)}", hits == len(sample),
              f"fallos: {misses[:3]}" if misses else "")
        if misses:
            d = conn.execute("""select count(*) from (select content_hash
                                from doc_chunks group by content_hash
                                having count(*) > 1) t""").fetchone()[0]
            print(f"       chunks con content_hash duplicado: {d}")

        print("\n4. CONSULTAS DE INVENTARIO")
        print("  'SUVs disponibles bajo $25,000 con menos de 60,000 millas'")
        rows = conn.execute("""
            select stock_number, vehicle, mileage, list_price_cents
            from search_inventory((select id from dealers limit 1),
                 p_body_type => 'suv', p_max_price_cents => 2500000,
                 p_max_mileage => 60000)""").fetchall()
        for r in rows:
            print(f"     {r[0]:10} {r[1]:34} {r[2]:>7,} mi  ${r[3]/100:>9,.0f}")
        print(f"     -> {len(rows)} resultados")

        print("\n  'pickups 2019 o mas nuevas con 4x4/4WD'")
        rows = conn.execute("""
            select stock_number, vehicle, drivetrain, list_price_cents
            from search_inventory((select id from dealers limit 1),
                 p_body_type => 'truck', p_year_min => 2019::smallint,
                 p_drivetrain => '4WD')""").fetchall()
        for r in rows:
            print(f"     {r[0]:10} {r[1]:34} {r[2][:18]:18} ${r[3]/100:>9,.0f}")
        print(f"     -> {len(rows)} resultados")

        print("\n5. v_inventory (10 filas, como la pantalla de un DMS)")
        print(f"     {'STOCK':10} {'VEHICLE':34} {'COND':5} {'MILES':>7} "
              f"{'PRICE':>11} {'DAYS':>4} {'PH':>3} STATUS")
        for r in conn.execute("select * from v_inventory limit 10").fetchall():
            print(f"     {r[0]:10} {r[1][:34]:34} {r[2]:5} {r[3]:>7,} "
                  f"{r[4]:>11} {r[5]:>4} {r[6]:>3} {r[7]}")

    print("\n" + ("TODO OK" if not FAIL else f"FALLOS: {FAIL}"))
    raise SystemExit(1 if FAIL else 0)


if __name__ == "__main__":
    main()
