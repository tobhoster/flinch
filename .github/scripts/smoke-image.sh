#!/usr/bin/env bash
# Runs a FLINCH image the way deploy/base runs it (uid 1000, read-only root
# filesystem, no capabilities, no privilege escalation) and checks what users
# rely on: the UI serves; the API refuses without a login and answers a
# session cookie or the API key, and no cache may keep its answers; a login,
# a logout and a write with the cookie need the UI's header; the cookie is
# Secure behind an HTTPS ingress; logging out ends the session; failed logins
# pause logging in; out-of-range settings are refused; every response carries
# the security headers; a server with only the API key serves automations and
# keeps the UI shut; a server with no credentials refuses everything; and the
# daemon's binaries start.
#
#   .github/scripts/smoke-image.sh <image>
set -euo pipefail

image=${1:?usage: smoke-image.sh <image>}
username=smoke
password=smoke-test-pass
key=smoke-test-token
locked=flinch-smoke-locked
keyed=flinch-smoke-keyed
open=flinch-smoke-open
jar=$(mktemp)
# What the UI sends with a login, a logout and every write.
ui=(-H 'X-Flinch-Request: 1' -H 'Content-Type: application/json')

fail() {
  echo "FAIL: $*" >&2
  for name in "$locked" "$keyed" "$open"; do
    docker logs "$name" 2>&1 | sed "s/^/[$name] /" >&2 || true
  done
  exit 1
}
cleanup() {
  docker rm -f "$locked" "$keyed" "$open" >/dev/null 2>&1 || true
  rm -f "$jar"
}
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

# login <username> <password>: the login form's JSON body.
login() {
  jq -cn --arg username "$1" --arg password "$2" '{username: $username, password: $password}'
}

user=$(docker image inspect "$image" --format '{{.Config.User}}')
[ "$user" = "1000:1000" ] || fail "the image runs as '$user', want 1000:1000"

for binary in flinch-arrd flinch-fit; do
  docker run --rm --read-only --cap-drop ALL "$image" "$binary" --help >/dev/null || fail "$binary --help failed"
done

serve "$locked" 17911 -e FLINCH_WEB_USERNAME="$username" -e FLINCH_WEB_PASSWORD="$password" -e FLINCH_WEB_TOKEN="$key"
base=http://127.0.0.1:17911

status 200 "$base/"
status 200 "$base/healthz"
status 401 "$base/api/status"
status 401 -X POST "$base/api/run"
status 401 -X POST --data '{}' "$base/v1/systemone"
json "/api/session before logging in" '. == {"authenticated": false, "login_configured": true}' "$base/api/session"
header "a refused request" '^www-authenticate: Cookie realm="flinch"' "$base/api/items"
header "a refused request" '^www-authenticate: Bearer realm="flinch"$' "$base/api/items"

# A login another site's page could post through the browser (no UI header,
# text/plain) is refused before it is checked, as is one too large to read.
status 403 -X POST -H 'Content-Type: text/plain' --data "$(login "$username" "$password")" "$base/api/login"
status 413 "${ui[@]}" -X POST --data "$(login "$username" "$(printf '%*s' 5000 '' | tr ' ' x)")" "$base/api/login"

# People: a wrong login is refused; the right one sets the session cookie,
# without Secure over plain http and with it behind an HTTPS ingress.
status 401 "${ui[@]}" -X POST --data "$(login "$username" "wrong-$password")" "$base/api/login"
header "a login" '^set-cookie: flinch_session=[0-9a-f]{64}; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000$' \
  "${ui[@]}" -X POST --data "$(login "$username" "$password")" "$base/api/login"
header "a login over HTTPS" '^set-cookie: flinch_session=[0-9a-f]{64}; .*; Secure$' \
  "${ui[@]}" -H 'X-Forwarded-Proto: https' -X POST --data "$(login "$username" "$password")" "$base/api/login"
status 204 -c "$jar" "${ui[@]}" -X POST --data "$(login "$username" "$password")" "$base/api/login"
json "/api/status with the session" '.demo == true' -b "$jar" "$base/api/status"
header "/api/items with the session" '^cache-control: no-store$' -b "$jar" "$base/api/items"
json "/api/session after logging in" '.authenticated == true' -b "$jar" "$base/api/session"
status 403 -b "$jar" -X PUT -H 'Content-Type: application/json' --data '{"capacity_ceiling": 5}' "$base/api/settings"
status 400 -b "$jar" -X PUT "${ui[@]}" --data '{"capacity_ceiling": 5}' "$base/api/settings"

# Machines: the API key as X-Api-Key or as a bearer token, no UI header.
status 401 -H "X-Api-Key: wrong-$key" "$base/api/status"
status 401 -H "Authorization: Bearer wrong-$key" "$base/api/status"
json "/api/status with X-Api-Key" '.demo == true' -H "X-Api-Key: $key" "$base/api/status"
json "/api/status with the bearer key" '.demo == true' -H "Authorization: Bearer $key" "$base/api/status"
header "/api/items with the key" '^cache-control: no-store$' -H "X-Api-Key: $key" "$base/api/items"
status 400 -X PUT -H "X-Api-Key: $key" -H 'Content-Type: application/json' \
  --data '{"capacity_ceiling": 5}' "$base/api/settings"
item=$(curl -s -H "X-Api-Key: $key" "$base/api/items" | jq -r '[.[] | select(.forecast != null)][0].id')
ask=$(jq -cn --arg id "$item" '{state: $id, questions: {safe: {type: "noul"}}}')
json "System One with the key" '.answers.safe.noul | type == "number"' -X POST -H "Authorization: Bearer $key" --data "$ask" "$base/v1/systemone"

# Logging out needs the UI's header, then ends the session on the server, not
# only in the browser.
session=$(awk '$6 == "flinch_session" { print $7 }' "$jar")
[ -n "$session" ] || fail "the cookie jar holds no session"
status 403 -b "$jar" -X POST "$base/api/logout"
status 200 -b "$jar" "$base/api/status"
status 204 -b "$jar" -c "$jar" "${ui[@]}" -X POST "$base/api/logout"
status 401 -H "Cookie: flinch_session=$session" "$base/api/status"

# Five failed logins in a row pause logging in, even with the right password.
for _ in 1 2 3 4 5; do
  status 401 "${ui[@]}" -X POST --data "$(login "$username" "wrong-$password")" "$base/api/login"
done
status 429 "${ui[@]}" -X POST --data "$(login "$username" "$password")" "$base/api/login"
header "a paused login" '^retry-after: [0-9]+$' "${ui[@]}" -X POST --data "$(login "$username" "$password")" "$base/api/login"
status 200 -H "X-Api-Key: $key" "$base/api/status"

header "the UI" "^content-security-policy: .*frame-ancestors 'none'" "$base/"
header "the UI" '^x-frame-options: deny$' "$base/"
header "the UI" '^x-content-type-options: nosniff$' "$base/"
header "the UI" '^referrer-policy: no-referrer$' "$base/"

# Only the API key, as an install upgraded from the token has until the
# login is added: automations keep working, the UI stays shut and says why.
serve "$keyed" 17913 -e FLINCH_WEB_TOKEN="$key"
keyed_base=http://127.0.0.1:17913
json "/api/status with the bearer key alone" '.demo == true' -H "Authorization: Bearer $key" "$keyed_base/api/status"
status 202 -X POST -H "Authorization: Bearer $key" "$keyed_base/api/run"
json "the key-only session" '. == {"authenticated": false, "login_configured": false}' "$keyed_base/api/session"
json "the key-only refusal" '.login_configured == false' "$keyed_base/api/status"
status 401 "${ui[@]}" -X POST --data "$(login "$username" "$key")" "$keyed_base/api/login"

# With no login and no API key the API stays closed, and says why.
serve "$open" 17912
open_base=http://127.0.0.1:17912
status 401 "$open_base/api/status"
json "the unconfigured refusal" '.login_configured == false' "$open_base/api/status"
json "the unconfigured session" '. == {"authenticated": false, "login_configured": false}' "$open_base/api/session"
status 401 "${ui[@]}" -X POST --data "$(login "$username" "$password")" "$open_base/api/login"
status 401 -H "X-Api-Key: $key" "$open_base/api/status"

echo "smoke test passed: $image"
