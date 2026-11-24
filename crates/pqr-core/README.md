# pqr-core

Core library for PQRGUARD: Beta-quantile certificate math, Haar rotation,
1-bit residual codes, IVF build, certified scan, and falsification audit.

## Modules

| Module | Responsibility |
|---|---|
| `math` | `G_D(-epsilon)`, exact Beta quantile, per-neighbour budget `beta = delta (m+1) / k`, general Beta quantile for Clopper-Pearson. |
| `rotation` | Haar-random orthogonal matrix from Gaussian QR with sign correction; `P^T` application. |
| `code` | 1-bit code, prefix statistics `a`, `b`, `kappa`, prune predicate. |
| `index` | IVF k-means, residual encoding, packed codes, staged scan, adaptive radius, save/load. |
| `certificate` | Target validation, certificate and degraded records, `Certificate::verify` recomputation, schema `pqr.guard/v0.1`. |
| `audit` | Fresh-rotation replay, recall comparison, pooled and per-query gates. |
| `control` | Proposition 1 positive control placement. |

## Certificate contract

For a fixed non-adaptive query `q`, a secret rotation `P`, target `tau` and
`delta`:

```text
Pr[Recall(q) < tau] <= delta
```

The budget is `beta = delta (m + 1) / k` with `m = floor((1 - tau) k)`, or
`beta = delta / k` for `tau = 1`. The budget is split evenly over the configured
stages. Each stage sets `epsilon_l` by inverting

```text
G_D(-epsilon) = 0.5 * Pr[Beta(1/2, (D-2)/2) > epsilon^2 / (D-1)]
```

at `delta_l = beta / L`. The scan tests every unvisited vector in the triangle
range. A vector is pruned only when the code estimate plus the theorem slack
falls below the radius threshold. Vectors that pass every stage receive an exact
fp64 distance, and the radius shrinks to the k-th exact candidate distance.

## Invariants

- No `recall` field in certificates.
- `tau` in `(0, 1]`, `delta` in `(0, 1)`, `k > 0`, `dim >= 3`.
- `k > n_live` is an error, never a degraded certificate.
- Degraded records carry no claim and cannot be upgraded.
- The index stores a BLAKE3 seed commitment, never the seed.
- Verification checks the claim against the target fields; it is not a signature
  and does not authenticate the producer or the assumptions.
- Claim `beta` and `expected_recall` use abs/rel tolerance `1e-12`:
  `|actual - expected| <= 1e-12 * max(1, |expected|)`. Larger differences are
  rejected.
- L2 only; no inner-product path.

## Resource and mutation contract

- `save` writes all ten files to a fresh sibling staging directory, fsyncs each file and the directory, then swaps it into place under `.pqr-index.lock` (the parent directory must be writable). A failed swap restores the previous index. If a crash stops between the two renames, the previous index sits in sibling `.pqr-backup-<index-dir-name>`; the next `save` or `load` restores it and sweeps stale `.pqr-save-*` directories. `index_id` is unchanged.
- `audit` takes `&mut PqrIndex` and leaves it reencoded with the final audit rotation seed and a new `index_id`. `AuditReport::index_id` records the pre-audit identity; reload the index to resume using the original.
- `load` caps `dim` at 4096, `n` and `partitions` at 20000000, stages at 64, each data file at 1 GiB, and the manifest at 1 MiB. Files open without following symlinks on Unix; size is checked on the open handle and reads are bounded, so `IndexError::Limit` is returned before oversized allocation.
- `Seed::from_file` opens with `O_NOFOLLOW` on Unix, checks mode and size on the open handle, then parses raw bytes first and trims only as a fallback. Service and CLI share this loader.

## Testing

```sh
cargo test -p pqr-core --release
cargo run -p pqr-core --release --example control
```

Covered seams: Beta quantile against independent quadrature, Lemma 1 tail law,
Proposition 1 positive control including weakened epsilon, exact query versus
brute force, save/load identity, corrupted index rejection, seed absence from
index files, certificate schema, degraded paths, and audit gates.
