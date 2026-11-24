# PQRGUARD

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)

PQRGUARD is a Rust index and CLI that answers k-nearest-neighbour queries and
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
- Per-query certificate JSON with theorem, budget, stages, assumptions, status;
  `verify --certificates` checks claim self-consistency.
- Fresh-rotation audit with pooled and per-query Clopper-Pearson gates.
- USearch HNSW benchmark baseline; PQR never certifies a graph result.
- Seed custody: file, environment, or KMS-command source; the index stores only a BLAKE3 commitment to the 32-byte seed.
- Degraded paths for adaptive queries, public seeds, filtered search, and
  approximate re-ranking; a degraded result never carries a certificate.

## Prerequisites

- Rust 1.85 or newer.
- Python 3 with NumPy to create `.npy` fixtures.

## Quick start

```sh
head -c 32 /dev/urandom > seed.bin
chmod 600 seed.bin

python -c "import numpy as np; rng=np.random.default_rng(1); X=rng.normal(size=(2000,16)).astype(np.float32); np.save('vectors.npy',X); np.save('queries.npy',X[:20])"

cargo run --release --bin pqrguard -- build \
  --vectors vectors.npy --partitions 20 --code-bits 8,16 \
  --seed-file seed.bin --out index/

cargo run --release --bin pqrguard -- query \
  --index index/ --queries queries.npy --seed-file seed.bin \
  --k 10 --tau 1 --delta 0.01 --nprobe 2 \
  --certificates certs.jsonl --results hits.jsonl

cargo run --release --bin pqrguard -- audit \
  --index index/ --queries queries.npy --seed-file seed.bin \
  --k 10 --tau 1 --delta 0.05 --rotations 64 --nprobe 2 \
  --report audit.json

cargo run --release --bin pqrguard -- bench \
  --index index/ --queries queries.npy --seed-file seed.bin \
  --k 10 --tau 1 --delta 0.01 --nprobe 2 --repeat 5 \
  --baseline usearch-hnsw
```

`certificates` is JSONL: one certified or degraded record per query. `query`
exits `2` when any query is degraded, and `audit` exits `3` when the gate fails.

`pqrguard verify --certificates certs.jsonl` checks each JSONL line and prints
a JSON summary. It exits `2` if any line is invalid, including degraded records
and certificates with edited fields. Verification is self-consistency only: it
does not authenticate who produced the certificate, and v0.1 has no signature.

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

Source notes. "Paper" is PQR-Search, *Per-Query Recall Guarantees for
Approximate Nearest Neighbor Search in Dynamic Vector Databases*,
[arXiv:2611.07579v2](https://arxiv.org/abs/2611.07579v2). The Proposition 1
placement comes from that paper and its reference implementation,
[suanlab/pqr-search](https://github.com/suanlab/pqr-search). The 0.5-1.5% bands
are this repo's acceptance tolerance for that check, not a table printed
verbatim in the paper. The paper's own aggregate audit is 45 failures in
4,176,000 trials (at most 0.5% of delta per setting), reported in Section 6.2.
The weakened-epsilon bands are the corresponding local detection bands.

SIFT10k (10,000 x 128, 100 queries, k=10, tau=1, delta=0.01):

| Check | Result |
|---|---|
| Certified query exactness | 100/100 queries equal brute force |
| Fresh-rotation audit, 1,024 rotations, delta=0.01 | 31 failures / 102,400 trials = 0.030%, max CP upper 0.0089, 0 queries over delta, pass |
| SIFT1M audit, 10 queries, 64 rotations, delta=0.05 | 1 failure / 640 trials, max CP upper 0.0720, pooled pass |
| Audit inference | falsification-only |

Single-thread latency on Apple M2, SIFT10k, 100 queries, k=10, tau=1, delta=0.01, nprobe=2:

| Metric | PQR | USearch HNSW (efSearch 32, recall 0.996) | Ratio | Gate |
|---|---:|---:|---:|---|
| p50 | 0.113 ms | 0.0406 ms | 2.79x | <= 4x |
| p95 | 0.159 ms | 0.0543 ms | 2.93x | <= 5x |

The 10k latency gate passes. PQR-Search
([arXiv:2611.07579v2](https://arxiv.org/abs/2611.07579v2), Section 6.3) reports
2.5x HNSW with AVX-512 C++ kernels. This portable Rust v0.1 uses byte lookup
tables with eight independent accumulators, norm-sorted partitions, and an
in-memory scan; its measured ratio is 2.79x on SIFT10k. On SIFT1M (2000 partitions, one 128-bit stage, nprobe=1,
`--iterations 5`) the build took 1054 s; a certified query with four scan
workers took 1.79 ms p50 and 2.67 ms p95 versus 0.659 ms and 0.899 ms for
single-thread USearch HNSW at efSearch 128, recall 0.998, using
`fixtures/sift1m.usearch` (the bench rebuilds it deterministically when absent).
That is 2.72x p50 and 2.97x p95, so the 4x gate is met at 1M. The comparison is
four PQR workers against single-thread HNSW. The 1M index is 539 MiB for 512 MB
of raw vectors, or 1.10x raw. A 100M run needs a disk-backed build: 100M x 128
fp32 is 51.2 GB of raw vectors against 24 GB of host RAM, and the public SIFT
bundle stops at 1M.

## Production

| Need | Source |
| --- | --- |
| Service runbook: deploy, auth, capacity, reload, backup, incidents | [`crates/pqr-service/README.md`](crates/pqr-service/README.md) |
| Operations detail: [threat model](crates/pqr-service/README.md#threat-model), [backup and recovery](crates/pqr-service/README.md#backup-and-recovery), [observability](crates/pqr-service/README.md#observability), [incident response](crates/pqr-service/README.md#incident-response) | [`crates/pqr-service/README.md`](crates/pqr-service/README.md) |
| Benchmark contract and gates | [`benchmarks/README.md`](benchmarks/README.md) |
| Container, Compose, and systemd deployment | [`deploy/README.md`](deploy/README.md) |
| Release artifact verification: checksums, signatures, attestations, SBOM | [`.github/workflows/README.md#verify-a-release-artifact`](.github/workflows/README.md#verify-a-release-artifact) |

The service has no built-in TLS and certificates carry no signature. Terminate
TLS at a proxy, prefer file or KMS seed custody over environment variables, and
treat certificates as self-consistent claims rather than authenticated
attestations.

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
- A known rotation lets an adversary choose always-pruned queries; the failure
  probability approaches 1. Keep the seed out of the index, logs, and repository.
- Choosing a query from a prior result breaks independence; `--adaptive`
  returns a degraded record.
- Maintaining the index from observed results also breaks the independence
  condition; v0.1 accepts no such updates.
- A graph backend cannot enumerate missed neighbours; PQR never certifies one.
- Inner product is unsupported. Normalise vectors before build and query for
  cosine similarity; the certificate remains an L2 statement.
- A seed commitment is not a secret-sharing scheme. Use 32 random bytes and
  keep them in the chosen custody source.
- Certificate verification recomputes the claim from the target fields. It does
  not attest to the stated assumptions or the producer's identity; there is no
  signature in v0.1.

## License

Apache License 2.0. See [LICENSE](LICENSE).
