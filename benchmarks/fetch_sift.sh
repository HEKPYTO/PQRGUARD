#!/usr/bin/env bash
# Fetch the canonical SIFT1M bundle and derive the pinned benchmark arrays.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
FIXTURES="$ROOT/fixtures"
TAR="$FIXTURES/sift.tar.gz"
TAR_URL="ftp://ftp.irisa.fr/local/texmex/corpus/sift.tar.gz"

TAR_SHA="92f1270c5e3a0cb46b89983e72b0511e4df065c31a9fa0276d8c9b1fca5bc81a"
BASE_SHA="4921953796bc066d3a4be1c6b26e32a27b8931080f21afe30824a63e65cb768d"
Q_SHA="5fc93ea99161f07c6f3f64a865433555219caa77e0425461b6628422af0c5466"
GT_SHA="bbb2f029fd7322e259650ce6acd72b01ecf1e4b973b9b9a7bd3633ed7f733af7"

usage() {
  cat <<'EOF'
Usage: benchmarks/fetch_sift.sh

Download ftp://ftp.irisa.fr/local/texmex/corpus/sift.tar.gz when absent, verify
its SHA256, and derive/verify these pinned arrays in fixtures/:

  sift1m.npy        (1000000, 128) float32
  sift1m_q100.npy   (100, 128) float32
  sift1m_gt100.npy  (100, 10) int64

Any checksum mismatch is fatal. No mirror or unverified fallback is used.
EOF
}

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

verify() {
  local path=$1 expected=$2 label=$3 actual
  if [[ ! -f "$path" ]]; then
    printf 'missing %s: %s\n' "$label" "$path" >&2
    return 1
  fi
  actual=$(sha256_file "$path")
  if [[ "$actual" != "$expected" ]]; then
    printf 'checksum mismatch for %s\n  want %s\n  got  %s\n' "$label" "$expected" "$actual" >&2
    return 1
  fi
  printf 'verified %s: %s\n' "$label" "$actual"
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
  "") ;;
  *) printf 'unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
esac

command -v curl >/dev/null 2>&1 || { echo 'curl is required' >&2; exit 2; }
python3 -c 'import numpy, tarfile' >/dev/null 2>&1 || {
  echo 'python3 with numpy is required' >&2; exit 2;
}
mkdir -p "$FIXTURES"

if ! verify "$TAR" "$TAR_SHA" "sift.tar.gz"; then
  echo "downloading $TAR_URL" >&2
  tmp_tar=$(mktemp "${TMPDIR:-/tmp}/sift.tar.gz.XXXXXX")
  trap 'rm -f "$tmp_tar"' EXIT
  curl -fL --retry 3 --retry-delay 2 -o "$tmp_tar" "$TAR_URL"
  verify "$tmp_tar" "$TAR_SHA" "downloaded sift.tar.gz"
  mv "$tmp_tar" "$TAR"
  trap - EXIT
fi

# Rebuild only a missing or mismatched array; output is byte-stable np.save.
python3 - "$TAR" "$FIXTURES" "$BASE_SHA" "$Q_SHA" "$GT_SHA" <<'PY'
import hashlib
import os
import pathlib
import sys
import tarfile
import tempfile

import numpy as np

tar_path = pathlib.Path(sys.argv[1])
out_dir = pathlib.Path(sys.argv[2])
expected = {
    "sift1m.npy": sys.argv[3],
    "sift1m_q100.npy": sys.argv[4],
    "sift1m_gt100.npy": sys.argv[5],
}


def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def read_vecs(member, dtype):
    with tarfile.open(tar_path, "r:gz") as tf:
        raw = tf.extractfile(member).read()
    width = int(np.frombuffer(raw, dtype=np.int32, count=1)[0])
    words = np.frombuffer(raw, dtype=np.int32).reshape(-1, width + 1)
    if not np.all(words[:, 0] == width):
        raise SystemExit(f"bad vector header in {member}")
    return np.ascontiguousarray(words[:, 1:].view(dtype))


def write_pinned(array, name):
    fd, tmp = tempfile.mkstemp(dir=out_dir, prefix=f".{name}.", suffix=".tmp")
    os.close(fd)
    try:
        with open(tmp, "wb") as fh:
            np.save(fh, array, allow_pickle=False)
        got = digest(tmp)
        if got != expected[name]:
            raise SystemExit(f"{name} derivation checksum mismatch: {got}")
        os.replace(tmp, out_dir / name)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)
    print(f"derived {name}: {got}")


for name, want in expected.items():
    target = out_dir / name
    if target.exists() and digest(target) == want:
        print(f"verified {name}: {want}")
        continue
    if name == "sift1m.npy":
        array = read_vecs("sift/sift_base.fvecs", np.float32)
    elif name == "sift1m_q100.npy":
        array = read_vecs("sift/sift_query.fvecs", np.float32)[:100]
    else:
        array = read_vecs("sift/sift_groundtruth.ivecs", np.int32)[:100, :10].astype(np.int64)
    write_pinned(array, name)
PY
