#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 [--binary PATH]" >&2
}

BINARY=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --binary)
      if [ "$#" -lt 2 ]; then
        usage
        exit 2
      fi
      BINARY="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/pqr-smoke.XXXXXX")"
CONTAINER=""
IMAGE=""
PID=""

cleanup() {
  if [ -n "$PID" ]; then
    kill "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  if [ -n "$CONTAINER" ]; then
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  fi
  if [ -n "$IMAGE" ]; then
    docker image rm "$IMAGE" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

CLI="$ROOT/target/release/pqrguard"
if [ ! -x "$CLI" ]; then
  cargo build --locked -p pqr-cli --manifest-path "$ROOT/Cargo.toml" >/dev/null
  CLI="$ROOT/target/debug/pqrguard"
fi

printf '11%.0s' {1..32} > "$TMP/seed.hex"
chmod 600 "$TMP/seed.hex"
python3 - "$TMP/vectors.npy" <<'PY'
import struct
import sys

rows, dim = 16, 4
header = "{'descr': '<f4', 'fortran_order': False, 'shape': (%d, %d), }" % (rows, dim)
padding = 16 - ((10 + len(header) + 1) % 16)
header_bytes = (header + " " * padding + "\n").encode("latin1")
vectors = b"".join(
    struct.pack("<f", float((row * dim + column) * 0.125))
    for row in range(rows)
    for column in range(dim)
)
with open(sys.argv[1], "wb") as output:
    output.write(b"\x93NUMPY\x01\x00")
    output.write(struct.pack("<H", len(header_bytes)))
    output.write(header_bytes)
    output.write(vectors)
PY
"$CLI" build --vectors "$TMP/vectors.npy" --seed-file "$TMP/seed.hex" \
  --code-bits 2,4 --partitions 4 --iterations 2 --out "$TMP/index" >/dev/null

PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
BASE="http://127.0.0.1:${PORT}"

if [ -z "$BINARY" ] && command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  IMAGE="pqr-serve:smoke-$$"
  docker build -f "$ROOT/deploy/Dockerfile" -t "$IMAGE" "$ROOT" >/dev/null
  CONTAINER="pqr-smoke-$$"
  docker run --rm -d --name "$CONTAINER" \
    --user "$(id -u):$(id -g)" \
    -v "$TMP/index:/var/lib/pqr/index:ro" \
    -v "$TMP/seed.hex:/run/secrets/pqr_seed:ro" \
    -e PQR_SEED_FILE=/run/secrets/pqr_seed \
    -e PQR_API_TOKEN=smoke-token \
    -e PQR_BIND=0.0.0.0:8080 \
    -p "127.0.0.1:${PORT}:8080" "$IMAGE" >/dev/null
else
  if [ -z "$BINARY" ]; then
    cargo build --locked -p pqr-service --manifest-path "$ROOT/Cargo.toml" >/dev/null
    BINARY="$ROOT/target/debug/pqr-serve"
  fi
  PQR_INDEX="$TMP/index" PQR_SEED_FILE="$TMP/seed.hex" \
    PQR_BIND="127.0.0.1:${PORT}" PQR_API_TOKEN=smoke-token \
    "$BINARY" >"$TMP/service.log" 2>&1 &
  PID=$!
fi

ready=0
for _ in $(seq 1 100); do
  if [ "$(curl -fsS "${BASE}/healthz" 2>/dev/null)" = "ok" ] \
    && [ "$(curl -fsS "${BASE}/readyz" 2>/dev/null)" = "ready" ]; then
    ready=1
    break
  fi
  sleep 0.1
done
if [ "$ready" -ne 1 ]; then
  echo "service did not become ready" >&2
  if [ -n "$PID" ]; then
    tail -n 40 "$TMP/service.log" >&2 || true
  fi
  if [ -n "$CONTAINER" ]; then
    docker logs "$CONTAINER" >&2 2>&1 || true
  fi
  exit 1
fi

assert_contains() {
  case "$1" in
    *"$2"*) ;;
    *)
      echo "missing $2" >&2
      return 1
      ;;
  esac
}

QUERY='{"query":[0.25,0.5,0.75,1.0],"k":3,"tau":1.0,"delta":0.1,"nprobe":2}'
RESPONSE="$(curl -fsS -H 'Authorization: Bearer smoke-token' \
  -H 'Content-Type: application/json' -d "$QUERY" "${BASE}/v1/query")"
assert_contains "$RESPONSE" '"status":"certified"'
assert_contains "$RESPONSE" '"certificate"'

METRICS="$(curl -fsS -H 'Authorization: Bearer smoke-token' "${BASE}/metrics")"
assert_contains "$METRICS" 'pqr_http_requests_total{route="/v1/query"'
assert_contains "$METRICS" 'pqr_query_duration_seconds_count 1'
if [ "$(curl -sS -o /dev/null -w '%{http_code}' "${BASE}/metrics")" != "401" ]; then
  echo "unauthenticated metrics request was not rejected" >&2
  exit 1
fi

echo "smoke ok: ${BASE}"
