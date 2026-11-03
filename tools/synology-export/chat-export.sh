#!/bin/sh
# Exports Synology Chat for teamcraft-import (docs/import-synology.md).
#
# Run on the NAS as root (it reads the Chat package's Postgres database read-only and copies the
# files), or against a restored dump on any host with --metadata-only and PSQL set:
#
#   sh chat-export.sh /volume1/chat-export
#   PSQL="psql -h localhost -U me -d synochat_restored -At" sh chat-export.sh --metadata-only ./export
#
# Output: NDJSON files (one JSON object per line), avatars/, stickers/, a pg_dump of the whole
# database for reference, raw/uploads/{channel_id}/{post_id} (the attachments) and DONE.
set -eu

METADATA_ONLY=0
if [ "${1:-}" = "--metadata-only" ]; then
  METADATA_ONLY=1
  shift
fi
OUT=${1:?usage: chat-export.sh [--metadata-only] <output directory>}
PSQL=${PSQL:-psql -U postgres -d synochat -At}
CHAT_DIR=${CHAT_DIR:-/volume1/chat/@ChatWorking}
APP_DIR=${APP_DIR:-/volume1/@appstore/Chat}
mkdir -p "$OUT"

q() {
  $PSQL -c "$1"
}

echo "[1/9] channels (all but type 4, Office; members are the current ones only)"
q "SELECT json_build_object('id',c.id,'name',c.name,'display_name',c.display_name,
     'purpose',c.purpose,'type',c.type,'create_at',c.create_at,'close_at',c.close_at,
     'creator_id',c.creator_id,
     'members',(SELECT coalesce(json_agg(m.user_id ORDER BY m.user_id),'[]'::json)
                FROM channel_members m WHERE m.channel_id=c.id AND m.delete_at IS NULL))
   FROM channels c WHERE c.type<>4 ORDER BY c.id" > "$OUT/channels.ndjson"

echo "[2/9] users (DSM accounts, deleted ones included with their flag)"
q "SELECT json_build_object('id',id,'username',username,'nickname',nickname,'type',type,
     'avatar_version',avatar_version,'deleted',(delete_at IS NOT NULL))
   FROM users_dsm ORDER BY id" > "$OUT/users.ndjson"

echo "[3/9] bots, chatbots and webhooks that wrote posts (names only, never tokens)"
q "SELECT json_build_object('id',u.id,'name',u.nickname,'kind',u.tableoid::regclass::text)
   FROM users u WHERE u.id NOT IN (SELECT id FROM users_dsm) ORDER BY u.id" > "$OUT/bots.ndjson"

echo "[4/9] favourites and notification settings"
q "SELECT json_build_object('channel_id',channel_id,'user_id',user_id)
   FROM channel_members WHERE is_star=1 AND delete_at IS NULL" > "$OUT/favorites.ndjson"
q "SELECT json_build_object('channel_id',channel_id,'user_id',user_id,
     'desktop',props->>'notification_desktop','mute',(props->>'notification_mute')::bool)
   FROM channel_members WHERE delete_at IS NULL AND props IS NOT NULL" > "$OUT/notifications.ndjson"

echo "[5/9] read state (last_view_at in ms: everything before it was read)"
q "SELECT json_build_object('channel_id',channel_id,'user_id',user_id,'last_view_at',last_view_at)
   FROM channel_members WHERE delete_at IS NULL AND last_view_at IS NOT NULL" > "$OUT/reads.ndjson"

echo "[6/9] webhooks, to recreate as bridge sources (tokens are left out)"
q "SELECT json_build_object('id',id,'name',nickname,'channel_id',channel_id,
     'disabled',is_disabled,'create_at',create_at)
   FROM webhook_incoming WHERE delete_at IS NULL ORDER BY channel_id,id" > "$OUT/webhooks_incoming.ndjson"
q "SELECT json_build_object('id',id,'name',nickname,'channel_id',channel_id,
     'url',url,'trigger_word',trigger_word,'disabled',is_disabled,'create_at',create_at)
   FROM webhook_outgoing WHERE delete_at IS NULL ORDER BY channel_id,id" > "$OUT/webhooks_outgoing.ndjson"

echo "[7/9] reactions (gemoji short codes)"
q "SELECT json_build_object('post_id',p.id,'channel_id',p.channel_id,
     'user_id',uid::int,'name',rj->>'sticker_name')
   FROM posts p
   JOIN channels c ON c.id=p.channel_id,
   LATERAL json_array_elements(p.reactions) AS er(rj),
   LATERAL json_array_elements_text(rj->'user_ids') AS ru(uid)
   WHERE c.type<>4 AND p.delete_at IS NULL AND json_typeof(p.reactions)='array'" > "$OUT/reactions.ndjson"

echo "[8/9] posts (type 1 text, 2 file, 3 sticker; not deleted; in channel and time order)"
q "SELECT json_build_object('id',p.id,'channel_id',p.channel_id,'user_id',p.user_id,
     'type',p.type,'message',p.message,'create_at',p.create_at,'update_at',p.update_at,
     'thread_id',p.thread_id,'comment_count',p.comment_count,'mentions',p.mentions,
     'file_props',p.file_props)
   FROM posts p JOIN channels c ON c.id=p.channel_id
   WHERE c.type<>4 AND p.type IN (1,2,3) AND p.delete_at IS NULL
   ORDER BY p.channel_id, p.create_at, p.id" > "$OUT/posts.ndjson"

if [ "$METADATA_ONLY" = 1 ]; then
  date -u +%Y-%m-%dT%H:%M:%SZ > "$OUT/DONE"
  echo "=== metadata done ==="
  exit 0
fi

echo "[9/9] avatars, sticker pictures, database dump and attachments"
mkdir -p "$OUT/avatars" "$OUT/stickers" "$OUT/raw"
q "SELECT id FROM users_dsm WHERE delete_at IS NULL AND avatar_version>0" | while IFS= read -r uid; do
  [ -n "$uid" ] || continue
  [ -f "$CHAT_DIR/avatar/u$uid" ] && cp -p "$CHAT_DIR/avatar/u$uid" "$OUT/avatars/$uid"
done
q "SELECT s.name||'|'||replace(s.path,'{0}','2x')
   FROM stickers s JOIN sticker_categories c ON c.id=s.category_id WHERE c.is_emoji=false" \
| while IFS='|' read -r name rel; do
  [ -n "$name" ] || continue
  [ -f "$APP_DIR/$rel" ] && cp -p "$APP_DIR/$rel" "$OUT/stickers/$name.png"
done
pg_dump -U postgres -Fc -f "$OUT/synochat.pgdump" synochat
cp -a --reflink=auto "$CHAT_DIR/uploads" "$OUT/raw/uploads"

( cd "$OUT" && find . -type f | wc -l ) > "$OUT/file-count.txt"
date -u +%Y-%m-%dT%H:%M:%SZ > "$OUT/DONE"
echo "=== done ==="
wc -l "$OUT"/*.ndjson
du -sh "$OUT"
