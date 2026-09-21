#!/usr/bin/env bash
# WHAT: takes the demo database from nothing to loaded and verified, in one go.
# WHY:  a demo you cannot rebuild is a demo you are afraid to touch. This throws
#       the volume away every time, so the result never depends on what happened
#       in a previous run.
# HOW:  down -v (drops the named volume) -> up -> wait for the healthcheck ->
#       migrate -> load -> verify. Any step failing stops the script.
set -euo pipefail
cd "$(dirname "$0")/.."

[ -f .env ] || { echo "falta .env - copia .env.example y ajustalo"; exit 1; }
set -a; . ./.env; set +a

PY="${PYTHON:-/home/leoiburn/.venv/bin/python}"

echo "==> borrando contenedor y volumen"
docker compose down -v --remove-orphans

echo "==> levantando Postgres"
docker compose up -d

echo -n "==> esperando healthcheck"
for _ in $(seq 1 60); do
  if [ "$(docker inspect -f '{{.State.Health.Status}}' automotrix-db 2>/dev/null)" = healthy ]; then
    echo " listo"; break
  fi
  echo -n "."; sleep 1
done
[ "$(docker inspect -f '{{.State.Health.Status}}' automotrix-db)" = healthy ] \
  || { echo " la base nunca quedo healthy"; docker compose logs --tail=40 db; exit 1; }

echo "==> migraciones"; "$PY" scripts/migrate.py
echo "==> carga";       "$PY" scripts/load.py
echo "==> verificacion"; "$PY" scripts/verify.py
echo "==> base lista en ${DATABASE_URL}"
