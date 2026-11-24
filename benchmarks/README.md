# Public 1M benchmark contract

This directory defines one reproducible SIFT1M measurement: PQR-GUARD certified
query latency versus a pinned single-thread USearch HNSW baseline. It pins the
dataset, derived arrays, seed, baseline graph, build/query flags, machine class,
gates, and report schema. Results from a different configuration are not
results under this contract.

## Dataset

Canonical source: INRIA TEXMEX
`ftp://ftp.irisa.fr/local/texmex/corpus/sift.tar.gz`.
HTTP `403` and HTTPS certificate failures on the same host are not a reason to
substitute an unverified mirror. `fetch_sift.sh` downloads only this URL, checks
the tar checksum before installing it, and aborts on any mismatch. No silent
fallback exists.

| File | SHA256 |
|---|---|
| `fixtures/sift.tar.gz` | `92f1270c5e3a0cb46b89983e72b0511e4df065c31a9fa0276d8c9b1fca5bc81a` |
| `fixtures/sift1m.npy` | `4921953796bc066d3a4be1c6b26e32a27b8931080f21afe30824a63e65cb768d` |
| `fixtures/sift1m_q100.npy` | `5fc93ea99161f07c6f3f64a865433555219caa77e0425461b6628422af0c5466` |
| `fixtures/sift1m_gt100.npy` | `bbb2f029fd7322e259650ce6acd72b01ecf1e4b973b9b9a7bd3633ed7f733af7` |

The tar members use fvecs/ivecs layout: each record starts with an `int32`
dimension, followed by that many values. `fetch_sift.sh` derives the pinned
arrays deterministically when one is missing or mismatched, then rechecks every
checksum before returning:

- `sift1m.npy`: `sift/sift_base.fvecs`, 1,000,000 x 128, `float32`.
- `sift1m_q100.npy`: first 100 rows of `sift/sift_query.fvecs`, 100 x 128, `float32`.
- `sift1m_gt100.npy`: first 100 rows, first 10 columns of
  `sift/sift_groundtruth.ivecs`, cast to `int64`.

## Run

From the repository root:

```sh
benchmarks/fetch_sift.sh
benchmarks/run_1m.sh
benchmarks/run_1m.sh --reuse-index     # keep an existing index at target/bench-1m/index
benchmarks/run_1m.sh --dry-run         # verify pins, print commands, execute nothing
PQR_BENCH_DIR=/path/to/output benchmarks/run_1m.sh
```

Requirements: Rust 1.85+, Python 3 with NumPy, `curl`, and `shasum` (or
`sha256sum`). `run_1m.sh` calls `fetch_sift.sh`, writes the pinned seed file,
builds the binary, builds the index, runs the benchmark, then runs the gate
checker. Its exit status is nonzero on any gate breach or input mismatch.

| Stage | Exact setting |
|---|---|
| Cargo | `cargo build --release -p pqr-cli` (workspace profile: `lto = thin`, `codegen-units = 1`) |
| Index vectors | `fixtures/sift1m.npy`, `--dim 128 --metric l2` |
| Index build | `--partitions 2000 --code-bits 128 --iterations 5` |
| Seed | 32-byte file from pinned hex below |
| Queries | `fixtures/sift1m_q100.npy`, 100 queries |
| Query target | `--k 10 --tau 1 --delta 0.01 --nprobe 1 --repeat 5` |
| Baseline | `--baseline usearch-hnsw --hnsw-cache fixtures/sift1m.usearch` |
| Gate checker | `--p50-max 4 --p95-max 5 --recall-min 0.99 --ef-search 128` |

The seed is the 64-character hex encoding of
`sha256("pqr-guard benchmark seed v1")`:

```text
f6d4447d718efc8069ebd632aad19f61783489666e160d135f0a81154efea736
```

`run_1m.sh` writes these 32 bytes to `<output>/seed.bin` for both index build
and bench. This seed is public **for benchmarking only**; production indexes
must use a secret seed, since a known rotation voids the certificate's security
claim. Historical numbers in the root `README.md` are context, not output of
this contract, and are not claimed to use this seed.

### HNSW cache provenance

`fixtures/sift1m.usearch` is checked in and pinned:

```text
sha256: c49c9ad5b3671200b18d58a9b49e73eaaf7ca7ae73d126e446676f95495d11fe
```

It was produced by the CLI's `usearch` 2.26 baseline from the same 1,000,000
base vectors in id order with `MetricKind::L2sq`, `ScalarKind::F32`,
connectivity 32, and expansion_add 200. There is no public URL for this cache.
The runner verifies the checksum and refuses to rebuild a mismatched graph
silently. To regenerate after a dependency upgrade, build a fresh index with
the pinned seed, then run the bench with a new cache path:

```sh
pqr-guard bench --index target/bench-1m/index \
  --queries fixtures/sift1m_q100.npy --seed-file target/bench-1m/seed.bin \
  --k 10 --tau 1 --delta 0.01 --nprobe 1 --repeat 5 \
  --baseline usearch-hnsw --hnsw-cache /tmp/sift1m-new.usearch
shasum -a 256 /tmp/sift1m-new.usearch
```

Record the new hash in this README before claiming any new result. A new cache
invalidates previous ratios. The CLI tunes `efSearch` upward from 16 until
average recall reaches 0.99; the contract requires the pinned cache to select
`efSearch = 128` and measure recall >= 0.99.

## Machine and interpretation

Reference machine: Apple M2, 8 logical cores (4 performance + 4 efficiency),
24 GB RAM. Keep the same machine, release profile, pinned cache, and pinned
inputs for comparable numbers.

For indices with at least 100,000 vectors, the current implementation scans
unvisited partitions with four hard-coded worker threads; HNSW search is
single-thread. The p50 and p95 gates therefore compare four PQR workers against
one HNSW worker. This is the defined contract, not a claim of equal-thread
efficiency. The HNSW graph is a reference only: PQR never certifies graph
results. The 4x/5x gates are this portable Rust v0.1's acceptance bands.

## Gates and output

`benchmarks/check_ratio.py` fails when `hnsw_ef_search != 128`, HNSW recall is
below 0.99, `pqr_p50_ms / hnsw_p50_ms > 4`, or
`pqr_p95_ms / hnsw_p95_ms > 5`. It also checks that the report describes 100
queries, k=10, repeat=5, and the HNSW baseline. A failed gate exits nonzero;
no gate is relaxed automatically.

`bench --report` writes `target/bench-1m/bench-report.json` with this schema:

| Field | Type | Meaning |
|---|---|---|
| `queries` | integer | Number of query rows |
| `k` | integer | Neighbours requested |
| `repeat` | integer | Query repetitions |
| `pqr_p50_ms` | float | PQR p50 latency, milliseconds |
| `pqr_p95_ms` | float | PQR p95 latency, milliseconds |
| `brute_p50_ms` | float or null | Brute-force p50 (null for HNSW baseline) |
| `speedup_over_brute` | float or null | Brute p50 / PQR p50 |
| `hnsw_p50_ms` | float | Baseline p50 latency |
| `hnsw_p95_ms` | float | Baseline p95 latency |
| `hnsw_ef_search` | integer | Selected baseline efSearch |
| `hnsw_recall` | float | Baseline recall against brute force |
| `pqr_over_hnsw` | float | Report's p50 ratio |
| `baseline` | string | `usearch-hnsw` for this contract |

The checker computes the p95 ratio from the two p95 fields; that ratio is not in
the report. A passing run prints a one-line JSON summary with `status: "pass"`
and both ratios.

## Beyond 1M

10M and 100M benchmarks are not implemented. The same commands, seed policy,
HNSW baseline options, machine class, and gates apply once the build is
disk-backed. Current limitations are explicit:

- `PqrIndex` holds vectors and auxiliary structures in memory; the build writes
  index files only at the end. There is no mmap-backed or streaming build, no
  disk-backed k-means, and no shard/merge path.
- The public SIFT bundle stops at 1M. This repository has no 10M/100M fetcher,
  checksums, queries, ground truth, or HNSW cache.
- 100M x 128 fp32 is 51.2 GB of raw vectors against 24 GB RAM; 10M is 5.12 GB
  before codes, centroids, and HNSW overhead. Do not extrapolate 1M timings or
  publish synthetic 10M/100M results as if measured.
- `fetch_sift.sh` derives arrays in memory, so it is a 1M-only helper.

No result above 1M is claimed by this contract.
