#!/usr/bin/env bash
# Deploy dry run: runs the playbook end to end against a throwaway local target and checks the
# result. The target is a systemd container (Debian trixie, test/target.Containerfile) on its own
# dual-stack Docker network; the playbook reaches it over SSH like a VPS.
#
#   test/dry-run.sh up        network, registry, target, forwarders, generated secrets
#   test/dry-run.sh images    build server + SFU images from this checkout, push to the test registry
#   test/dry-run.sh deploy    run the playbook (twice: the second run must change nothing)
#   test/dry-run.sh verify    health over IPv4 + IPv6, internal API refused, SFU over QUIC (v4 + v6),
#                             messages between two dev users (teamcraft e2e harness)
#   test/dry-run.sh down      remove everything again
#   test/dry-run.sh all       up, images, deploy, verify (leaves the target running)
#
# Needs: docker (the target runs privileged, for nested rootless podman), podman (image builds),
# ansible-playbook, openssl, socat, curl with HTTP/3, ssh, and the dev stack running
# (docker compose -f docker-compose.dev.yml up -d keycloak garage, from the repository root).
#
# Secrets are generated into $DRY_RUN_DIR (default ~/.cache/teamcraft-dry-run), never into git.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
# The repository root (deploy/ lives in it); images and the e2e harness come from there.
teamcraft_repo="${TEAMCRAFT_REPO:-$(cd "$repo/.." && pwd)}"
state="${DRY_RUN_DIR:-$HOME/.cache/teamcraft-dry-run}"

network=tc-dryrun
subnet4=172.31.250.0/24
subnet6=fd00:7c:fa::/64
gateway4=172.31.250.1
registry_ip=172.31.250.5
target_ip4=172.31.250.10
target_ip6=fd00:7c:fa::10
target=tc-dryrun-target
registry=tc-dryrun-registry
domain=chat.172-31-250-10.sslip.io
sfu_domain=sfu.172-31-250-10.sslip.io
# Dev stack ports on the control machine's loopback, forwarded into the target.
forwarded_ports=(8080 3900)
forward_offset=10000

step() { printf '\n==> %s\n' "$*"; }

wait_until() { # description, timeout seconds, command...
  local what="$1" timeout="$2"; shift 2
  local deadline=$((SECONDS + timeout))
  until "$@" >/dev/null 2>&1; do
    if [ "$SECONDS" -ge "$deadline" ]; then echo "timed out waiting for $what" >&2; return 1; fi
    sleep 2
  done
}

up() {
  mkdir -p "$state"
  chmod 700 "$state"
  step "Network $network ($subnet4, $subnet6)"
  docker network inspect "$network" >/dev/null 2>&1 \
    || docker network create --ipv6 --subnet "$subnet4" --gateway "$gateway4" --subnet "$subnet6" "$network"

  step "Registry"
  docker container inspect "$registry" >/dev/null 2>&1 \
    || docker run -d --name "$registry" --network "$network" --ip "$registry_ip" docker.io/library/registry:3

  step "Target"
  docker build -q -t teamcraft-dry-run-target:local -f "$here/target.Containerfile" "$here"
  if ! docker container inspect "$target" >/dev/null 2>&1; then
    # /home on a volume: the services' podman storage needs a real filesystem, not overlayfs.
    docker run -d --name "$target" --hostname dry-run-target --privileged --cgroupns=private \
      --network "$network" --ip "$target_ip4" --ip6 "$target_ip6" \
      --tmpfs /run --tmpfs /run/lock -v tc-dryrun-home:/home \
      teamcraft-dry-run-target:local
  fi
  wait_until "the target's systemd" 60 docker exec "$target" systemctl is-system-running --wait

  step "SSH key"
  [ -f "$state/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C teamcraft-dry-run -f "$state/id_ed25519"
  docker exec -i "$target" sh -c 'cat > /home/deploy/.ssh/authorized_keys && chown -R deploy:deploy /home/deploy/.ssh && chmod 600 /home/deploy/.ssh/authorized_keys' < "$state/id_ed25519.pub"

  step "Test registry is trusted inside the target (plain HTTP)"
  docker exec "$target" sh -c "mkdir -p /etc/containers/registries.conf.d && printf '[[registry]]\nlocation = \"$registry_ip:5000\"\ninsecure = true\n' > /etc/containers/registries.conf.d/50-dry-run.conf"

  forwarders

  step "Secrets ($state/secrets.yaml)"
  [ -f "$state/secrets.yaml" ] || generate_secrets
}

# control machine: <gateway>:1xxxx -> 127.0.0.1:xxxx; target: 127.0.0.1:xxxx -> <gateway>:1xxxx
forwarders() {
  step "Forward the dev stack (Keycloak, Garage) into the target"
  local port
  for port in "${forwarded_ports[@]}"; do
    local outer=$((port + forward_offset))
    if ! pgrep -f "socat TCP-LISTEN:$outer,bind=$gateway4" >/dev/null; then
      nohup socat "TCP-LISTEN:$outer,bind=$gateway4,reuseaddr,fork" "TCP:127.0.0.1:$port" \
        >"$state/forward-$port.log" 2>&1 &
    fi
    docker exec "$target" sh -c "cat > /etc/systemd/system/dry-run-forward-$port.service <<UNIT
[Unit]
Description=Dry run: dev stack port $port from the control machine
[Service]
ExecStart=/usr/bin/socat TCP-LISTEN:$port,bind=127.0.0.1,reuseaddr,fork TCP:$gateway4:$outer
Restart=always
[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload && systemctl enable --now dry-run-forward-$port.service"
  done
}

generate_secrets() {
  local key="$state/call-signing.pem"
  openssl genpkey -algorithm ed25519 -out "$key" 2>/dev/null
  {
    echo "postgres_password: $(openssl rand -hex 24)"
    # The dev stack's Garage keys (teamcraft.example.toml); they protect nothing.
    echo "s3_access_key: GKdeadbeef0000000000000000"
    echo "s3_secret_key: $(printf 'deadbeef%.0s' 1 2 3 4 5 6 7 8)"
    echo "sfu_registration_token: $(openssl rand -hex 32)"
    echo "meetings_guest_token_secret: $(openssl rand -base64 48 | tr -d '\n')"
    echo "call_signing_key: |"
    sed 's/^/  /' "$key"
    echo "call_signing_public_key: |"
    openssl pkey -in "$key" -pubout | sed 's/^/  /'
  } > "$state/secrets.yaml"
  chmod 600 "$state/secrets.yaml" "$key"
}

images() {
  step "Images from $teamcraft_repo ($(git -C "$teamcraft_repo" rev-parse --short HEAD))"
  local name
  for name in teamcraft-server teamcraft-sfu; do
    podman build --format docker -f "$teamcraft_repo/containers/$name.Containerfile" -t "localhost/$name:dry-run" "$teamcraft_repo"
    podman push --tls-verify=false --digestfile "$state/$name.digest" \
      "localhost/$name:dry-run" "docker://$registry_ip:5000/zeitgleich/$name:dry-run"
    echo "$name $(cat "$state/$name.digest")"
  done
}

deploy() {
  local args=(-i "$repo/ansible/inventories/dry-run" "$repo/ansible/site.yml"
    --private-key "$state/id_ed25519"
    -e "teamcraft_secrets_file=$state/secrets.yaml"
    -e "teamcraft_server_digest=$(cat "$state/teamcraft-server.digest")"
    -e "teamcraft_sfu_digest=$(cat "$state/teamcraft-sfu.digest")")
  if [ -n "${TEAMCRAFT_WEB_ARCHIVE:-}" ]; then args+=(-e "teamcraft_web_archive=$TEAMCRAFT_WEB_ARCHIVE"); fi
  step "Playbook"
  ANSIBLE_CONFIG="$here/ansible.cfg" ansible-playbook "${args[@]}"
  step "Playbook again (idempotence: nothing may change)"
  ANSIBLE_CONFIG="$here/ansible.cfg" ansible-playbook "${args[@]}" | tee "$state/second-run.log"
  if ! grep -Eq 'changed=0 +unreachable=0 +failed=0' "$state/second-run.log"; then
    echo "the second run changed something" >&2
    return 1
  fi
}

verify() {
  local ca="$state/caddy-root.crt"
  docker exec "$target" cat /home/tc-proxy/caddy-data/caddy/pki/authorities/local/root.crt > "$ca"
  local failed=0
  check() { # description, command...
    local what="$1"; shift
    if "$@" >/dev/null 2>&1; then echo "ok    $what"; else echo "FAIL  $what"; failed=1; fi
  }
  # QUIC checks need a curl built with HTTP/3 (Fedora's is; Ubuntu's isn't): skipped, not failed.
  check_http3() { # description, command...
    if curl -V | grep -q HTTP3; then check "$@"; else echo "skip  $1 (curl without HTTP/3)"; fi
  }
  step "Checks"
  # shellcheck disable=SC2016 # expands inside the target, not here
  check "services active" docker exec "$target" sh -c '
    for pair in tc-db:postgres tc-proxy:caddy tc-server:teamcraft-server tc-sfu:teamcraft-sfu; do
      user=${pair%%:*}; unit=${pair#*:}; uid=$(id -u "$user")
      XDG_RUNTIME_DIR=/run/user/$uid runuser -u "$user" -- systemctl --user is-active "$unit.service" || exit 1
    done'
  local ip
  for ip in "$target_ip4" "[$target_ip6]"; do
    local resolve="$domain:443:${ip//[\[\]]/}"
    check "healthz over $ip" curl -fsS --cacert "$ca" --resolve "$resolve" "https://$domain/healthz"
    check "readyz over $ip" curl -fsS --cacert "$ca" --resolve "$resolve" "https://$domain/readyz"
    check "internal API refused over $ip" sh -c "[ \"\$(curl -s -o /dev/null -w '%{http_code}' --cacert '$ca' --resolve '$resolve' -X POST https://$domain/api/internal/v1/calls/participants/joined)\" = 404 ]"
    check_http3 "HTTP/3 to Caddy over $ip" curl -fsS --http3-only --cacert "$ca" --resolve "$resolve" "https://$domain/healthz"
    # The QUIC handshake with a verified certificate proves UDP 4433 reachable and the exported
    # certificate in use; the SFU then closes the plain HTTP/3 request (it only speaks WebTransport).
    check_http3 "SFU QUIC + TLS over $ip" sh -c "curl -v -m 10 --http3-only --cacert '$ca' --resolve '$sfu_domain:4433:${ip//[\[\]]/}' https://$sfu_domain:4433/ 2>&1 | grep -q 'SSL certificate verified'"
  done
  # shellcheck disable=SC2016 # expands inside the target, not here
  check "SFU restarts for a renewed certificate" docker exec "$target" sh -c '
    uid=$(id -u tc-sfu); run() { XDG_RUNTIME_DIR=/run/user/$uid runuser -u tc-sfu -- "$@"; }
    before=$(run systemctl --user show -p ExecMainPID --value teamcraft-sfu.service)
    touch /srv/teamcraft/certs/sfu.crt; sleep 5
    after=$(run systemctl --user show -p ExecMainPID --value teamcraft-sfu.service)
    [ "$before" != "$after" ] && run systemctl --user is-active teamcraft-sfu.service'
  check "SFU control port not public" sh -c "! curl -s -m 3 http://$target_ip4:8081/healthz"
  check "server not public (only through Caddy)" sh -c "! curl -s -m 3 http://$target_ip4:8090/healthz && ! curl -s -m 3 http://$target_ip4:8080/healthz"
  check "Postgres not public" sh -c "! (exec 3<>/dev/tcp/$target_ip4/5432) 2>/dev/null"
  step "Messages between two dev users (teamcraft e2e harness)"
  if (cd "$teamcraft_repo" && SSL_CERT_FILE="$ca" E2E_DEPLOYED_URL="https://$domain" SQLX_OFFLINE=true \
      cargo test -q -p teamcraft-e2e --features e2e --test suite -- --exact deployed::a_deployed_instance_is_healthy_and_delivers_messages --nocapture); then
    echo "ok    e2e: login and encrypted messages both ways"
  else
    echo "FAIL  e2e: login and encrypted messages both ways"; failed=1
  fi
  return "$failed"
}

down() {
  step "Removing target, registry, network, forwarders"
  docker rm -f "$target" "$registry" >/dev/null 2>&1 || true
  docker volume rm tc-dryrun-home >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
  pkill -f "socat TCP-LISTEN:[0-9]*,bind=$gateway4" || true
  podman rmi localhost/teamcraft-server:dry-run localhost/teamcraft-sfu:dry-run >/dev/null 2>&1 || true
  echo "kept: $state (secrets, SSH key); remove it by hand"
}

case "${1:-}" in
  up) up ;;
  images) images ;;
  deploy) deploy ;;
  verify) verify ;;
  down) down ;;
  all) up; images; deploy; verify ;;
  *) sed -n '2,20p' "$0"; exit 2 ;;
esac
