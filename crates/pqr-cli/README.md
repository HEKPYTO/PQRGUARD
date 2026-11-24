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

### verify

```sh
pqrguard verify --certificates certs.jsonl
```

Checks every JSONL line: schema, certified status, target, stages, strict
stage ordering, and the claim recomputed from `k`, `tau`, and `delta`. Bare
certificate objects and `query` output lines are accepted; degraded records
are not. Prints a JSON summary with line, valid, invalid counts and errors.
Exit `2` if any line is invalid. This checks self-consistency only: it does not
authenticate the producer, attest to the assumptions, or verify a signature.
Numeric claims allow abs/rel tolerance `1e-12` for platform rounding; larger
differences are rejected.

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
`--rotations` must be positive. Exit `3` on audit failure, `4` on configuration
or I/O errors.

### bench

```sh
pqrguard bench --index idx/ --queries q.npy --seed-file seed.bin \
  --k 10 --tau 1 --delta 0.01 --nprobe 2 --repeat 5 \
  --baseline usearch-hnsw --hnsw-cache hnsw.usearch --report bench.json
```

`--baseline brute` times exact brute force. `--baseline usearch-hnsw` builds a
USearch HNSW graph with connectivity 32 and expansion 200, tunes `efSearch` to
average recall at least 0.99, and reports p50, p95, recall, and the PQR/HNSW
ratio. `--hnsw-cache` reuses a serialized USearch graph across runs. HNSW is a
baseline only; it is never certified. Use `--baseline none` to time only PQR.

## Seed custody

Supply exactly one seed source:

- `--seed-file PATH`: 32 raw bytes or 64 hex characters. On Unix the file must
  not be a symlink and must have no group or other permission bits (`0600` or
  stricter).
- `--seed-env VAR`: 32 raw bytes or 64 hex characters.
- `--seed-command CMD`: run `sh -c CMD`; stdout must be 32 raw bytes or 64 hex
  characters. Use this to call a KMS CLI.

The seed never appears in the index, certificate, report, stdout, or logs. The
index manifest stores only `blake3(seed)`. A wrong seed is rejected with
`SeedMismatch`. The audit master seed is derived from the index seed with a
domain separator unless `--audit-seed-file` is supplied.

## Exit codes

| Code | Meaning |
|---:|---|
| 0 | Success, certified, or audit pass |
| 2 | At least one degraded query, or an invalid certificate line in `verify` |
| 3 | Audit failure |
| 4 | Invalid configuration, corrupt index, dimension mismatch, I/O error |
