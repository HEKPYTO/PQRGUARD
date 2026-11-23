# Examples

## control

Reproduces the Proposition 1 positive control and the weakened-epsilon audit
check.

```sh
cargo run --release -p pqr-core --example control
```

Expected output, on the committed implementation:

```text
D=64 k=1 tau=1 rotations=100000 failures=967 rate=0.00967
D=64 k=10 tau=0.9 rotations=100000 failures=1014 rate=0.01014
D=64 k=10 tau=1 rotations=100000 failures=1014 rate=0.01014
D=256 k=100 tau=1 rotations=10000 failures=87 rate=0.00870
weakened delta=0.02 failures=1964 rate=0.01964
weakened delta=0.05 failures=4863 rate=0.04863
```

The first four rows land in the paper's positive-control range. The last two
show that deliberately weakened error bounds are detected.
