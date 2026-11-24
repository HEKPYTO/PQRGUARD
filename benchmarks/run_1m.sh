#!/usr/bin/env bash
# Reproducible 1M PQR-vs-HNSW latency contract. Fails on any gate breach.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
BENCH="$ROOT/benchmarks"
FIXTURES="$ROOT/fixtures"
OUT_DIR=${PQR_BENCH_DIR:-"$ROOT/target/bench-1m"}
case "$OUT_DIR" in
  /*) ;;
  *) OUT_DIR="$PWD/$OUT_DIR" ;;
esac
INDEX_DIR="$OUT_DIR/index"
SEED_FILE="$OUT_DIR/seed.bin"
REPORT="$OUT_DIR/bench-report.json"
BIN="$ROOT/target/release/pqr-guard"
HNSW_CACHE="$FIXTURES/sift1m.usearch"

# Benchmark-only seed: public on purpose, never use for production indexes.
SEED_HEX="f6d4447d718efc8069ebd632aad19f61783489666e160d135f0a81154efea736"
HNSW_CACHE_SHA="c49c9ad5b3671200b18d58a9b49e73eaaf7ca7ae73d126e446676f95495d11fe"

usage() {
  cat <<'EOF'
Usage: benchmarks/run_1m.sh [--reuse-index] [--dry-run]

Builds pqr-guard in release mode, builds a 2000-partition SIFT1M index with
5 k-means iterations, then benches 100 queries at --nprobe 1 --repeat 5 against
the pinned USearch HNSW cache. The report is written to
target/bench-1m/bench-report.json (override with PQR_BENCH_DIR).

  --reuse-index  skip the index build (seed and config must still match)
  --dry-run      print the exact commands and verify local pins; run nothing

Gates: p50/HNSW <= 4, p95/HNSW <= 5, hnsw_ef_search == 128, HNSW recall >= 0.99.
Exit status is nonzero on any gate failure or missing/mismatched input.
EOF
}

reuse=0
dry=0
while (($#)); do
  case "$1" in
    --reuse-index) reuse=1 ;;
    --dry-run) dry=1 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

run() {
  printf '+'
  printf ' %q' "$@"
  printf '\n'
  if (( dry == 0 )); then
    "$@"
  fi
}

if [[ ! -f "$HNSW_CACHE" ]]; then
  printf 'missing pinned HNSW cache: %s\n' "$HNSW_CACHE" >&2
  exit 2
fi
cache_sha=$(sha256_file "$HNSW_CACHE")
if [[ "$cache_sha" != "$HNSW_CACHE_SHA" ]]; then
  printf 'HNSW cache checksum mismatch\n  want %s\n  got  %s\n' "$HNSW_CACHE_SHA" "$cache_sha" >&2
  exit 2
fi
printf 'verified HNSW cache: %s\n' "$cache_sha"

run "$BENCH/fetch_sift.sh"
mkdir -p "$OUT_DIR"

if (( dry )); then
  printf '+ write %s with pinned 32-byte seed\n' "$SEED_FILE"
else
  python3 - "$SEED_FILE" "$SEED_HEX" <<'PY'
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_bytes(bytes.fromhex(sys.argv[2]))
PY
fi

run cargo build --release --manifest-path "$ROOT/Cargo.toml" -p pqr-cli

if (( reuse )); then
  printf 'reusing index: %s\n' "$INDEX_DIR"
else
  run "$BIN" build \
    --vectors "$FIXTURES/sift1m.npy" --dim 128 --metric l2 \
    --partitions 2000 --code-bits 128 --iterations 5 \
    --seed-file "$SEED_FILE" --out "$INDEX_DIR"
fi

run "$BIN" bench \
  --index "$INDEX_DIR" --queries "$FIXTURES/sift1m_q100.npy" \
  --seed-file "$SEED_FILE" --k 10 --tau 1 --delta 0.01 \
  --nprobe 1 --repeat 5 --baseline usearch-hnsw \
  --hnsw-cache "$HNSW_CACHE" --report "$REPORT"

run python3 "$BENCH/check_ratio.py" "$REPORT" \
  --p50-max 4 --p95-max 5 --recall-min 0.99 --ef-search 128
