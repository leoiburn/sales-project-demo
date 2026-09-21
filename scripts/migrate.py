"""
WHAT: Applies the .up.sql files in /migrations, in version order, once each.
WHY:  The schema must come from migrations only. Mounting SQL into
      docker-entrypoint-initdb.d looks easier but runs solely on a brand-new
      volume and is skipped silently ever after, so the database and the repo
      drift apart without anyone noticing.
HOW:  Tracks applied versions in schema_migrations. Each migration runs in its
      own transaction, so a failure leaves the earlier ones applied and the
      broken one rolled back.

  python scripts/migrate.py          apply pending migrations
  python scripts/migrate.py down     revert the newest applied migration

The files use the sqlx-cli naming convention (<version>_<name>.up.sql /
.down.sql), so `sqlx migrate run` can take over once cargo is installed.
"""
import glob
import os
import re
import sys

import psycopg

DSN = os.environ["DATABASE_URL"]
MIG_DIR = os.path.join(os.path.dirname(__file__), "..", "migrations")


def versions():
    out = []
    for p in sorted(glob.glob(os.path.join(MIG_DIR, "*.up.sql"))):
        name = os.path.basename(p)
        m = re.match(r"(\d+)_(.+)\.up\.sql$", name)
        out.append((int(m.group(1)), m.group(2), p))
    return out


def main():
    down = len(sys.argv) > 1 and sys.argv[1] == "down"
    with psycopg.connect(DSN) as conn:
        conn.execute("""
            create table if not exists schema_migrations (
                version     bigint primary key,
                description text not null,
                applied_at  timestamptz not null default now()
            )""")
        conn.commit()
        applied = {r[0] for r in conn.execute(
            "select version from schema_migrations").fetchall()}

        if down:
            if not applied:
                print("nada que revertir")
                return
            v = max(applied)
            _, desc, up = next(x for x in versions() if x[0] == v)
            sql = open(up.replace(".up.sql", ".down.sql")).read()
            conn.execute(sql)
            conn.execute("delete from schema_migrations where version = %s", (v,))
            conn.commit()
            print(f"revertido {v}_{desc}")
            return

        pending = [x for x in versions() if x[0] not in applied]
        if not pending:
            print("sin migraciones pendientes")
            return
        for v, desc, path in pending:
            conn.execute(open(path).read())
            conn.execute(
                "insert into schema_migrations (version, description) values (%s, %s)",
                (v, desc))
            conn.commit()
            print(f"aplicado {v}_{desc}")


if __name__ == "__main__":
    main()
