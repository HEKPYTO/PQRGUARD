# PQR-GUARD

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)

PQR-GUARD is a Rust index and CLI that answers k-nearest-neighbour queries and
returns a per-query recall certificate:

```text
Pr[Recall(q) < tau] <= delta
```

The probability is over a secret Haar-random rotation inside the index. It is
not observed recall, not a promise about a particular result list, and not a
graph-index claim. The index visits the nearest IVF partitions, sets the test
radius from their exact k-th distance, and tests every other vector with
1-bit residual codes under a union bound over the k true neighbours. Vectors
that pass all stages are re-ranked with exact fp64 distances.

## Features

- Exact Beta-quantile pruning thresholds; no normal approximation.
- IVF, Haar rotation, 1-bit prefix codes, triangle-inequality filter, adaptive
  radius, staged scan.
- Per-query certificate JSON with theorem, budget, stages, assumptions, status.
- Fresh-rotation audit with pooled and per-query Clopper-Pearson gates.
- USearch HNSW benchmark baseline; PQR never certifies a graph result.
- Seed custody: the index stores only a BLAKE3 commitment to the 32-byte seed.
- Degraded paths for adaptive queries, public seeds, filtered search, and
  approximate re-ranking; a degraded result never carries a certificate.

## Prerequisites

- Rust 1.85 or newer.
- Python 3 with NumPy to create `.npy` fixtures.

## Quick start

```sh
head -c 32 /dev/urandom > seed.bin

python -c "import numpy as np; rng=np.random.default_rng(1); X=rng.normal(size=(2000,16)).astype(np.float32); np.save('vectors.npy',X); np.save('queries.npy',X[:20])"

cargo run --release --bin pqr-guard -- build \
  --vectors vectors.npy --partitions 20 --code-bits 8,16 \
  --seed-file seed.bin --out index/

cargo run --release --bin pqr-guard -- query \
  --index index/ --queries queries.npy --seed-file seed.bin \
  --k 10 --tau 1 --delta 0.01 --nprobe 2 \
  --certificates certs.jsonl --results hits.jsonl

cargo run --release --bin pqr-guard -- audit \
  --index index/ --queries queries.npy --seed-file seed.bin \
  --k 10 --tau 1 --delta 0.05 --rotations 64 --nprobe 2 \
  --report audit.json

cargo run --release --bin pqr-guard -- bench \
  --index index/ --queries queries.npy --seed-file seed.bin \
  --k 10 --tau 1 --delta 0.01 --nprobe 2 --repeat 5 \
  --baseline usearch-hnsw
```

`certificates` is JSONL: one certified or degraded record per query. `query`
exits `2` when any query is degraded, and `audit` exits `3` when the gate fails.

## Results

Positive control, Proposition 1 placement, from
`cargo run --release -p pqr-core --example control`:

| Setting | Rotations | Failures | Rate | Paper range |
|---|---:|---:|---:|---|
| D=64, k=1, tau=1, delta=0.01 | 100,000 | 967 | 0.967% | 0.95-0.97% |
| D=64, k=10, tau=0.9, delta=0.01 | 100,000 | 1,014 | 1.014% | 0.5-1.5% |
| D=64, k=10, tau=1, delta=0.01 | 100,000 | 1,014 | 1.014% | 0.5-1.5% |
| D=256, k=100, tau=1, delta=0.01 | 10,000 | 87 | 0.870% | 0.5-1.5% |
| Weakened epsilon, delta'=0.02 | 100,000 | 1,964 | 1.964% | 1.87-1.98% |
| Weakened epsilon, delta'=0.05 | 100,000 | 4,863 | 4.863% | 4.69-4.86% |

SIFT10k (10,000 x 128, 100 queries, k=10, tau=1, delta=0.01):

| Check | Result |
|---|---|
| Certified query exactness | 100/100 queries equal brute force |
| Fresh-rotation audit, 64 rotations, delta=0.05 | 17 failures / 6,400 trials = 0.266%, pooled gate pass |
| Audit inference | falsification-only |

Single-thread latency on Apple M2, SIFT10k, 100 queries, k=10, nprobe=2:

| Metric | PQR | USearch HNSW (efSearch 32, recall 0.996) | Ratio | Gate |
|---|---:|---:|---:|---|
| p50 | 0.153 ms | 0.039 ms | 3.95x | <= 4x |
| p95 | 0.212 ms | 0.052 ms | 4.02x | <= 5x |
| Throughput vs exact brute force | 5.42x | - | - | - |

The paper reports 2.5x HNSW with AVX-512 C++ kernels. This portable Rust v0.1
uses scalar CRC-style popcount paths and byte lookup tables; its measured ratio
is 3.95x on the fixture above. Larger scales may widen the ratio.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --release -- -D warnings
cargo test --release
cargo run --release -p pqr-core --example control
```

CI runs format, clippy, and the release test suite. The nightly workflow runs a
1,000-rotation audit on a generated fixture.

## Documentation

- [`crates/pqr-core/README.md`](crates/pqr-core/README.md) - certificate math,
  index layout, scan invariants.
- [`crates/pqr-cli/README.md`](crates/pqr-cli/README.md) - commands, exit codes,
  seed custody.
- [`crates/pqr-core/examples/README.md`](crates/pqr-core/examples/README.md) -
  reproducible positive control.

## Trust boundary

- The rotation seed is secret. A public seed makes every certificate degraded.
- Queries and updates must be chosen independently of the rotation. Adaptive
  query sequences are degraded.
- The audit can falsify the bound; it cannot prove it.
- L2 only in v0.1. Cosine is supported by normalising inputs before build and
  query. Inner product is refused.
- The index is static in v0.1: there is no insert, delete, or merge API.
- A passing audit is evidence, not a theorem. The theorem is the certificate's
  warrant, subject to its stated assumptions.

## License

Apache License 2.0. See [LICENSE](LICENSE).
