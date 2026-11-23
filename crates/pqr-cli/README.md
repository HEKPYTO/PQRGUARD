# pqrguard

Command-line front end for PQRGUARD.

## Commands

### build

```sh
pqrguard build --vectors sift.npy --dim 128 --metric l2 \
  --partitions 100 --code-bits auto --seed-file seed.bin --out idx/
```

`--code-bits` accepts `auto`, one length, or a comma-separated increasing list.
`auto` uses `128,256,D` truncated to `D`. `--metric` accepts only `l2` in v0.1.

### query

```sh
pqrguard query --index idx/ --queries q.npy --seed-file seed.bin \
  --k 10 --tau 0.9 --delta 0.01 --nprobe 4 \
  --certificates certs.jsonl --results hits.jsonl
```

Exit `0` when every query is certified, `2` when any query is degraded.
`--adaptive`, `--public-seed`, `--filtered`, and `--approximate-rerank` force
degraded records without a claim.

### audit

```sh
pqrguard audit --index idx/ --queries q.npy --seed-file seed.bin \
  --k 10 --tau 1 --delta 0.05 --rotations 64 --nprobe 4 \
  --gt gt.npy --report audit.json
```

The index is re-encoded under fresh rotations derived from an audit master seed.
With fewer than 1,000 rotations the verdict uses the pooled failure-rate gate.
With 1,000 or more it uses per-query Clopper-Pearson: at most 0.5% of queries
above `delta`, none above `1.5 * delta`, and pooled rate at most `delta`.
Exit `3` on audit failure, `4` on configuration or I/O errors.

### bench

```sh
pqrguard bench --index idx/ --queries q.npy --seed-file seed.bin \
  --k 10 --tau 1 --delta 0.01 --nprobe 2 --repeat 5 \
  --baseline usearch-hnsw --report bench.json
```

`--baseline brute` times exact brute force. `--baseline usearch-hnsw` builds a
USearch HNSW graph with connectivity 32 and expansion 200, tunes `efSearch` to
average recall at least 0.99, and reports p50, p95, recall, and the PQR/HNSW
ratio. HNSW is a baseline only; it is never certified.

## Seed custody

`--seed-file` must contain exactly 32 bytes. The seed never appears in the index,
certificate, report, or logs. The index manifest stores only
`blake3(seed)`. A wrong seed is rejected with `SeedMismatch`.

## Exit codes

| Code | Meaning |
|---:|---|
| 0 | Success, certified, or audit pass |
| 2 | At least one degraded query |
| 3 | Audit failure |
| 4 | Invalid configuration, corrupt index, dimension mismatch, I/O error |
