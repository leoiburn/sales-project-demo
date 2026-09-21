#!/usr/bin/env bash
# WHAT: takes the demo database from nothing to loaded and verified, in one go.
# WHY:  a demo you cannot rebuild is a demo you are afraid to touch. This throws
#       the volume away every time, so the result never depends on what happened
#       in a previous run.
# HOW:  down -v (drops the named volume) -> up -> wait for the healthcheck ->
#       sqlx migrate run -> cargo run --bin load -> cargo run --bin verify.
#       Any step failing stops the script.
#
# Pass --rebuild-seed to regenerate the synthetic inventory and re-embed the
# corpus before loading (the first run downloads the ~440MB embedding model into
# .fastembed_cache/). Without it the committed seed/*.json and seed/*.ndjson are
# used as-is.
set -euo pipefail
cd "$(dirname "$0")/.."

[ -f .env ] || { echo "falta .env - copia .env.example y ajustalo"; exit 1; }
set -a; . ./.env; set +a

export PATH="$HOME/.cargo/bin:$PATH"
if [ "${1:-}" = "--rebuild-seed" ]; then
  echo "==> regenerando inventario y corpus"
  cargo run --release --quiet -p datagen --bin gen_inventory
  cargo run --release --quiet -p datagen --bin build_corpus
fi

echo "==> compilando el loader"
cargo build --release --quiet

echo "==> borrando contenedor y volumen"
docker compose down -v --remove-orphans

echo "==> levantando Postgres y Mailpit"
docker compose up -d db mailpit

echo -n "==> esperando healthcheck"
for _ in $(seq 1 60); do
  if [ "$(docker inspect -f '{{.State.Health.Status}}' automotrix-db 2>/dev/null)" = healthy ]; then
    echo " listo"; break
  fi
  echo -n "."; sleep 1
done
[ "$(docker inspect -f '{{.State.Health.Status}}' automotrix-db)" = healthy ] \
  || { echo " la base nunca quedo healthy"; docker compose logs --tail=40 db; exit 1; }

echo "==> migraciones"
sqlx migrate run --source migrations

echo "==> carga"
./target/release/load

echo "==> verificacion"
./target/release/verify

echo "==> base lista en ${DATABASE_URL}"
