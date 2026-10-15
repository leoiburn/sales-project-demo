#!/usr/bin/env bash
# WHAT: start the whole stack (Docker) with the machine's CURRENT LAN IP.
# WHY:  the laptop gets its IP by DHCP and it changes (it was .108, then .197).
#       A hardcoded PUBLIC_HOSTNAME / PUBLIC_BASE_URL in .env goes stale, so the
#       page is unreachable "via IP" and email links point at a dead address.
# HOW:  read the source IP the kernel would use and export it; exported shell
#       variables override .env in compose. Stop with scripts/down.sh.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! docker info >/dev/null 2>&1; then
  echo "Docker is not running. Start it with: sudo systemctl start docker" >&2
  exit 1
fi

port=$(grep -E '^HTTPS_PORT=' .env | cut -d= -f2)
port=${port:-8443}

ip=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' || true)
if [ -n "$ip" ]; then
  export PUBLIC_HOSTNAME="$ip"
  export PUBLIC_BASE_URL="https://$ip:$port"
fi

# --build: always run the current code; cached layers make it quick when nothing changed
docker compose up -d --build

echo "chat:  https://localhost:$port${ip:+   phones on the Wi-Fi: https://$ip:$port}"
echo "email: http://localhost:8025"
echo "stop:  scripts/down.sh"
