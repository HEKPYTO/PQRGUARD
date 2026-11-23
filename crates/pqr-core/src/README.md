# pqr-core source

Module order follows the certificate path:

1. `math.rs` - Beta quantile and failure budget.
2. `rotation.rs` - Haar rotation.
3. `code.rs` - 1-bit prefix statistics.
4. `index.rs` - IVF build, encode, staged scan, save/load.
5. `certificate.rs` - schema and target validation.
6. `audit.rs` - fresh-rotation replay and gates.
7. `control.rs` - Proposition 1 placement.

`index.rs` is the only module that performs I/O. The scan keeps squared
distances; the prune test works in unsquared geometry. `coeff_a`, `coeff_k`,
and `coeff_c` are precomputed per vector and stage so a query reads only packed
codes and lookup tables.
