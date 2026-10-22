#!/usr/bin/env bash
# Creates the CalCraft account the TeamCraft server writes meeting events with (dev stack only).
#
#   docker compose -f docker-compose.dev.yml up -d calcraft && dev/init-calcraft.sh
#
# Afterwards the server configuration is:
#   [meetings.caldav]
#   calendar_url = "http://127.0.0.1:5232/dav/calendars/teamcraft/default/"
#   username = "teamcraft"
#   password = "teamcraft-dev-only"
# The credentials are for local development only. Safe to run again.
set -euo pipefail

cd "$(dirname "$0")/.."
compose=${COMPOSE:-docker compose}
user=teamcraft
password=teamcraft-dev-only

if printf '%s\n' "$password" |
  $compose -f docker-compose.dev.yml exec -T calcraft /usr/local/bin/calcraft user add "$user" \
    --display-name TeamCraft --password-stdin 2>/dev/null; then
  echo "created CalCraft user $user"
else
  echo "CalCraft user $user exists already"
fi

url="http://127.0.0.1:5232/dav/calendars/$user/default/"
status=$(curl -s -o /dev/null -w '%{http_code}' -u "$user:$password" -X PROPFIND -H 'Depth: 0' "$url")
[ "$status" = 207 ] || { echo "calendar $url answered HTTP $status" >&2; exit 1; }
echo "calendar ready: $url"
