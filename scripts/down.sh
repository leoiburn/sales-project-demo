#!/usr/bin/env bash
# WHAT: stop the whole stack (Docker).
# WHY:  start and stop the demo whenever you want without losing anything.
# HOW:  `docker compose down` removes the containers and network but keeps the
#       named volumes (database, certificate, speech model), so scripts/up.sh
#       brings everything back as it was. Never add -v here: it deletes the data.
set -euo pipefail
cd "$(dirname "$0")/.."
docker compose down
