# pqr-service

HTTP frontend for `pqr-core`. `pqr-serve` loads a saved `PqrIndex` at startup and serves queries with per-query recall certificates. The service never logs seeds or query vectors.

## Run

```
pqr-serve --index /var/lib/pqr/index --seed-file /etc/pqr/seed.hex
```

Startup fails if the index cannot be loaded or the seed does not match the index commitment. Nothing is bound before that check succeeds.

## Configuration

Every flag has an equivalent environment variable. Exactly one seed source must be supplied.

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--index` | `PQR_INDEX` | required | Directory written by `pqr-guard build --out`. |
| `--seed-file` | `PQR_SEED_FILE` | none | 32 raw bytes or 64 hex characters. |
| `--seed-env` | `PQR_SEED_ENV` | none | Name of a variable holding the seed; value is ASCII-trimmed. |
| `--seed-command` | `PQR_SEED_COMMAND` | none | Shell command whose stdout is an ASCII-trimmed seed. |
| `--bind` | `PQR_BIND` | `127.0.0.1:8080` | Listener address. Non-loopback binds require a token. |
| `--api-token` | `PQR_API_TOKEN` | none | Bearer token for protected endpoints. |
| `--max-body-bytes` | `PQR_MAX_BODY_BYTES` | `65536` | Per-request body limit; larger bodies get 413. |
| `--query-timeout-ms` | `PQR_QUERY_TIMEOUT_MS` | `5000` | Wall-clock limit for a query; timeout gets 504. |
| `--max-k` | `PQR_MAX_K` | `1024` | Upper bound on requested `k`. |
| `--max-nprobe` | `PQR_MAX_NPROBE` | `64` | Upper bound on requested `nprobe`. |
| `--max-concurrency` | `PQR_MAX_CONCURRENCY` | `64` | Concurrent in-flight requests. |

Seed decoding matches `pqr-guard`: 32 raw bytes, or 64 hex characters for the env and command sources.

## Endpoints

- `GET /healthz` — liveness. Always `200 ok` while the process is up.
- `GET /readyz` — `200 ready` when the index is loaded and stable; `503 not ready` during or after a failed reload.
- `GET /metrics` — Prometheus text (protected when a token is configured).
- `POST /v1/query` — JSON body `{"query":[f32],"k":usize,"tau":f64,"delta":f64,"nprobe":usize}`. Optional `"public_seed":true` forces a degraded response.
- `POST /admin/reload` — reload the index directory atomically (protected when a token is configured).

Successful query response:

```json
{
  "hits": [{"id": 12, "distance2": 0.125}],
  "status": "certified",
  "certificate": {"status": "certified", "index_id": "...", "schema": "pqr.guard/v0.1"}
}
```

A degraded query returns `"status":"degraded"` and a `degraded` object with reasons. The `certificate` key is absent; no response is ever upgraded from degraded to certified. `k`, `tau`, `delta`, and `nprobe` are validated against the core target rules and the configured bounds. NaN, infinite values, dimension mismatch, malformed JSON, unknown fields, and oversized bodies are rejected.

## Auth and TLS boundary

When `PQR_API_TOKEN` is set, `/v1/query`, `/admin/reload`, and `/metrics` require `Authorization: Bearer <token>`. Comparison is constant time for equal-length tokens; token length is not treated as secret. Health and readiness probes stay public for orchestrators.

The service does not terminate TLS. A non-loopback bind without a token is refused. For remote access, terminate TLS at a reverse proxy and pass the bearer token through; do not send a token over plain HTTP on an untrusted network.

## Limits and observability

Bodies are capped before parsing, query work runs on the blocking pool under a wall-clock timeout, and concurrency is capped by a tower layer. Logs are JSON with method, path, status, and elapsed time; bodies, seeds, tokens, and query vectors are never logged. Set `RUST_LOG` to change the tracing filter.

Metrics:

- `pqr_http_requests_total{route,code}` — request count by route and status class (`2xx`, `4xx`, `5xx`).
- `pqr_http_errors_total{reason}` — errors by reason (`auth`, `timeout`, `client`, `server`).
- `pqr_query_duration_seconds` — histogram with buckets from 1 ms to 1 s plus `+Inf`, sum, and count.

## Backup and reload

Back up the whole index directory as written by `pqr-guard build`; files are a consistent set identified by the manifest `index_id`. Write a replacement index into a staging directory, then swap it into `PQR_INDEX` and send `POST /admin/reload` or `SIGHUP`. Reload marks readiness false, loads and verifies the new index, then swaps the `Arc<PqrIndex>` and marks readiness true. Queries arriving while not ready get 503 and no certificate.

## Failure modes

- Missing/invalid seed, mismatched commitment, corrupt index, or bad config: startup exits nonzero before binding.
- Failed reload: old index is kept but readiness stays false until a later successful reload; failure is logged and counted.
- Query error: input errors are 400, timeouts are 504, internal failures are 500, and an unavailable index is 503. None of these return certificates.
- SIGTERM/SIGINT: graceful drain of in-flight requests via `axum::serve`.

## Dependencies

- `axum` 0.8 — router, extractors, body limit, and graceful HTTP serving.
- `tokio` 1 — async runtime, TCP listener, signals, timeouts, and blocking query tasks.
- `serde` / `serde_json` — config-free JSON request/response shapes and Prometheus-safe response encoding.
- `thiserror` — typed startup/index errors.
- `tracing` / `tracing-subscriber` — structured JSON logs and `RUST_LOG` filtering.
- `tower` / `tower-http` — concurrency limit and request body limit layers.
- `clap` — flag parsing plus environment-variable defaults.
- `pqr-core` — index loading, query execution, and certificate types.

Development-only: `tempfile` for generated index fixtures.
