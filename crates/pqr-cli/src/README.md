# pqrguard source

`main.rs` contains the clap command tree and the four paths:

| Function | Command | Failure surface |
|---|---|---|
| `build` | `build` | input shape, metric, code-bit stages, seed length |
| `query` | `query` | target validation, query shape, seed match |
| `run_audit` | `audit` | ground-truth shape, rotation budget, gate verdict |
| `bench` | `bench` | baseline selection, repeat count |

Exit codes and seed custody are documented in `../README.md`.
