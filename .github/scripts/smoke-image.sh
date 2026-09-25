#!/usr/bin/env bash
# Runs a FLINCH image the way deploy/base runs it (uid 1000, read-only root
# filesystem, no capabilities, no privilege escalation) and checks what users
# rely on: the UI serves, the API refuses without the token and answers with
# it, out-of-range settings are refused, every response carries the security
# headers, a server without a token refuses everything, and the daemon's
# binaries start.
#
#   .github/scripts/smoke-image.sh <image>
set -euo pipefail

image=${1:?usage: smoke-image.sh <image>}
token=smoke-test-token
locked=flinch-smoke-locked
open=flinch-smoke-open

fail() {
  echo "FAIL: $*" >&2
  for name in "$locked" "$open"; do
    docker logs "$name" 2>&1 | sed "s/^/[$name] /" >&2 || true
  done
  exit 1
}
cleanup() { docker rm -f "$locked" "$open" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Start the demo on 127.0.0.1:<port> under the manifests' restrictions.
serve() {
  local name=$1 port=$2
  shift 2
  docker run -d --name "$name" -p "127.0.0.1:$port:7911" \
    --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges \
    -e FLINCH_STATE_DIR=/tmp/demo "$@" "$image" sh -c 'flinch-demo && exec flinch-web' >/dev/null
  for _ in $(seq 1 60); do
    curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  fail "$name did not become healthy"
}

# status <expected> <curl args...>: the HTTP status must match.
status() {
  local want=$1 got
  shift
  got=$(curl -s -o /dev/null -w '%{http_code}' "$@")
  [ "$got" = "$want" ] || fail "curl $* returned $got, want $want"
}

# json <what> <jq filter> <curl args...>: the JSON body must satisfy the filter.
json() {
  local what=$1 filter=$2
  shift 2
  curl -s "$@" | jq -e "$filter" >/dev/null || fail "$what: the body does not satisfy $filter"
}

# header <what> <regex> <curl args...>: a response header must match,
# ignoring case. Captured first: grep -q ending a pipeline early would trip
# pipefail.
header() {
  local what=$1 pattern=$2 headers
  shift 2
  headers=$(curl -s -D - -o /dev/null "$@" | tr -d '\r')
  grep -qiE "$pattern" <<<"$headers" || fail "$what has no header matching $pattern"
}

user=$(docker image inspect "$image" --format '{{.Config.User}}')
[ "$user" = "1000:1000" ] || fail "the image runs as '$user', want 1000:1000"

for binary in flinch-arrd flinch-fit; do
  docker run --rm --read-only --cap-drop ALL "$image" "$binary" --help >/dev/null || fail "$binary --help failed"
done

serve "$locked" 17911 -e FLINCH_WEB_TOKEN="$token"
base=http://127.0.0.1:17911

status 200 "$base/"
status 200 "$base/healthz"
status 401 "$base/api/status"
status 401 -H "Authorization: Bearer wrong-$token" "$base/api/status"
status 401 -X POST "$base/api/run"
status 401 -X POST --data '{}' "$base/v1/systemone"
status 200 -H "Authorization: Bearer $token" "$base/api/status"
json "/api/status with the token" '.demo == true' -H "Authorization: Bearer $token" "$base/api/status"
status 400 -X PUT -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  --data '{"capacity_ceiling": 5}' "$base/api/settings"

header "a refused request" '^www-authenticate: Bearer realm="flinch"$' "$base/api/items"
header "the UI" "^content-security-policy: .*frame-ancestors 'none'" "$base/"
header "the UI" '^x-frame-options: deny$' "$base/"
header "the UI" '^x-content-type-options: nosniff$' "$base/"
header "the UI" '^referrer-policy: no-referrer$' "$base/"

# Without a token the API stays closed, and says why.
serve "$open" 17912
status 401 "http://127.0.0.1:17912/api/status"
json "the unconfigured refusal" '.configured == false' "http://127.0.0.1:17912/api/status"

echo "smoke test passed: $image"
