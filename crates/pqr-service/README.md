# pqr-service

HTTP frontend for `pqr-core`. `pqr-serve` loads a saved `PqrIndex` at startup and
serves queries with per-query recall certificates. The service never logs seeds,
tokens, request bodies, or query vectors.

## Operations

### Deployment modes

| Mode | Bind | Token | TLS owner | Notes |
| --- | --- | --- | --- | --- |
| Local development | `127.0.0.1:8080` (default) | optional | none needed | Loopback only; the default bind is refused only when a non-loopback address has no token. |
| Production (recommended) | loopback, e.g. `127.0.0.1:8080` | required in practice | reverse proxy on the same host | Proxy terminates TLS, injects the bearer token, and is the only network-reachable listener. |
| Direct internal | non-loopback address | required by `Config::validate` | none | Plaintext on the wire. Use only on a trusted private network; token and queries are interceptable. |

The process has no TLS listener. For remote clients, terminate TLS at a proxy
and keep the proxy-to-service hop on loopback. If the proxy is on another host,
use a private network or mTLS between proxy and service; the service cannot
authenticate the proxy beyond the bearer token. Health and readiness probes are
public by design; restrict them at the proxy if the orchestrator does not need
them from the pod network.

### Configuration

Every flag has an equivalent environment variable. Exactly one seed source must be supplied.

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--index` | `PQR_INDEX` | required | Directory written by `pqr-guard build --out`. |
| `--seed-file` | `PQR_SEED_FILE` | none | 32 raw bytes or 64 hex characters; trailing ASCII whitespace allowed, no symlink, no group/world read access. |
| `--seed-env` | `PQR_SEED_ENV` | none | Name of a variable holding the seed; value is ASCII-trimmed. |
| `--seed-command` | `PQR_SEED_COMMAND` | none | Shell command whose stdout is an ASCII-trimmed seed. |
| `--bind` | `PQR_BIND` | `127.0.0.1:8080` | Listener address. Non-loopback binds require a token. |
| `--api-token` | `PQR_API_TOKEN` | none | Bearer token for protected endpoints; empty or whitespace-only values are rejected. |
| `--max-body-bytes` | `PQR_MAX_BODY_BYTES` | `65536` | Per-request body limit; larger bodies get 413. |
| `--query-timeout-ms` | `PQR_QUERY_TIMEOUT_MS` | `5000` | Wall-clock limit for a query, including blocking-pool queue time; timeout gets 504. |
| `--max-k` | `PQR_MAX_K` | `1024` | Upper bound on requested `k`. |
| `--max-nprobe` | `PQR_MAX_NPROBE` | `64` | Upper bound on requested `nprobe`. |
| `--max-concurrency` | `PQR_MAX_CONCURRENCY` | `64` | Concurrent in-flight requests; values above 64 are capped. |

Seeds are decoded by `pqr-core` and held in zeroizing memory. Raw 32-byte files
are kept byte-exact; hex and command/env values are ASCII-trimmed. Seed files
must be regular files with no symlink and no group or world read bits, and the
file reader rejects oversized files. Startup fails before binding if the index
cannot be loaded, the seed does not match the index commitment, or config is
invalid.

### Authentication and token rotation

Set `PQR_API_TOKEN` (preferred over the CLI flag, which can appear in process
listings) to a freshly generated high-entropy value, for example
`openssl rand -hex 32`. Store it in a secret manager, not in the repository or
in an index backup. Empty or whitespace-only tokens are rejected at startup and
on every request. The comparison is constant time for equal-length tokens;
token length is not treated as secret. `/v1/query`, `/admin/reload`, and
`/metrics` require `Authorization: Bearer <token>`; `/healthz` and `/readyz`
stay public. Rejected requests return 401 and increment
`pqr_http_errors_total{reason="auth"}`.

The service holds exactly one token, and `/admin/reload` reloads only the
index. Token changes need a process restart. For zero-downtime rotation, run two
pools: keep the old token on pool A, start pool B with the new token, then move
proxy traffic from A to B and stop A. A single-pool rotation has a short window
where either the old or new token is rejected. To revoke a token, remove it at
the proxy and restart the service with a different token; the old value stops
working as soon as no instance accepts it.

All protected endpoints share one token: possession grants query, metrics, and
admin reload. There is no role separation. Restrict `/admin/reload` and
`/metrics` at the proxy if clients should only reach `/v1/query`.

### Capacity, concurrency, and timeouts

The index is loaded fully into memory at startup. Budget RAM for the index plus
query working set; do not rely on swap for latency. The 1M-point index in the
root README is about 539 MiB for 512 MB of raw vectors.

- `PQR_MAX_CONCURRENCY` (default 64) caps in-flight requests with a tower layer.
  The effective cap is 64 even if a larger value is configured, keeping the
  blocking pool available. Excess requests **queue** rather than being
  rejected, so put a client or proxy deadline and rate limit in front of the
  service; otherwise the queue is unbounded.
- `PQR_QUERY_TIMEOUT_MS` (default 5000) is a wall-clock limit around a blocking
  query task and includes time spent waiting for a blocking-pool worker.
  Timeout returns 504 with no certificate and increments
  `pqr_http_errors_total{reason="timeout"}`. Set it below the proxy and client
  deadlines.
- `PQR_MAX_BODY_BYTES` (default 65536) caps the body before JSON parsing.
- `PQR_MAX_K` and `PQR_MAX_NPROBE`, plus the index dimension and live vector
  count, bound accepted queries. Rejected input returns 400 with no
  certificate.
- Stage epsilon bounds are derived once per `(index_id, k, tau, delta)` and
  cached (up to 1024 entries, then cleared). Clients cannot supply or observe
  them.

Use the ignored load test in `tests/load.rs` to establish a throughput and p95
baseline for the target hardware, then set concurrency below the point where
p95 exceeds the local latency target.

### Health and readiness

- `GET /healthz` — liveness. Always `200 ok` while the process is up. It does
  not check the index or dependencies.
- `GET /readyz` — readiness. `200 ready` when the index is loaded and stable;
  `503 not ready` during a reload or after a failed reload. Gate traffic on
  this endpoint.
- `GET /metrics` — Prometheus text (protected when a token is configured).

Use liveness to restart a wedged process and readiness to remove an instance
from the pool. A failed reload keeps the old index but leaves readiness false
until a later successful reload, so the instance stops receiving traffic.

### Graceful reload and rollback

Write a replacement index into a staging directory, then swap it into
`PQR_INDEX` and call `POST /admin/reload` or send `SIGHUP`. Reloads are
serialized with a try-lock: a concurrent `POST /admin/reload` returns 409
`reload already in progress`, and repeated `SIGHUP` signals are rate-limited to
one reload per second (a busy signal is logged as a warning). A reload sets
readiness false, loads and verifies the new directory (schema, seed commitment,
`index_id` checksum), swaps the in-memory `Arc<PqrIndex>`, and sets readiness
true. Queries arriving while not ready get 503 and no certificate. Poisoned
reload, index, and epsilon-cache locks are recovered with `into_inner()` rather
than leaving the service wedged. `SIGTERM` and `SIGINT` drain in-flight requests
through `axum::serve`.

There is no in-process version history. Rollback means pointing `PQR_INDEX` at
the previous known-good directory and reloading again. Keep the previous
directory until the new one has served traffic and passed verification.

### Seed custody

- Prefer `--seed-file` with a `0600` file owned by the service user, or
  `--seed-command` backed by a KMS that audits access. `--seed-env` (`PQR_SEED_ENV`
  names the variable) is the weakest custody option: environment values are
  visible to host processes, container inspection, and some crash dumps.
- `--seed-command` runs through `sh -c` with service privileges. Only use a
  trusted, pinned command.
- The index stores only a BLAKE3 commitment to the seed. A seed change requires
  rebuilding the index with `pqr-guard build`; swapping the seed alone fails
  startup with a commitment mismatch.
- Keep the seed out of the index backup, logs, repository, and tickets. Treat
  it as compromised if it appeared in process environment dumps.
- A seed file must be a regular file, not a symlink, with no group or world read
  bits. Trailing ASCII whitespace is trimmed before parsing; malformed seeds
  are rejected. Seed bytes are held in zeroizing memory.

### Failure modes

- Missing/invalid seed, mismatched commitment, corrupt index, or bad config:
  startup exits nonzero before binding.
- Empty or whitespace-only `PQR_API_TOKEN`: config validation fails before
  binding; an empty token is also denied at request time.
- Seed file symlink, group/world-readable permissions, or invalid length/hex:
  startup exits nonzero.
- Concurrent reload: `POST /admin/reload` returns 409; a busy `SIGHUP` is logged
  and skipped. Poisoned locks are recovered, not fatal.
- Failed reload: old index is kept but readiness stays false until a later
  successful reload; the failure is logged and counted as a server error.
- Query error: input errors are 400, timeouts are 504, internal failures are
  500, and an unavailable index is 503. None of these return certificates.
- Unauthorized request: 401 on protected routes; health and readiness remain
  public.
- SIGTERM/SIGINT: graceful drain of in-flight requests.

## Threat model

### Assets

| Asset | Why it matters |
| --- | --- |
| Rotation seed (32 bytes) | Secret basis of every certificate. Leakage lets an adversary craft always-pruned queries and invalidates the probability bound. |
| Index directory | Contains vectors, codes, seed commitment, and `index_id`. Tampering changes results; disclosure leaks the corpus. |
| Certificate | The per-query claim `Pr[Recall(q) < tau] <= delta`. Integrity is self-consistency only. |
| Signing key | External release signing identity (Sigstore keyless OIDC identity and the verifier's trust root). It protects artifacts, not certificates; the service holds no key. |
| Service | Availability and correctness of `/v1/query`; also holds the token, seed, and index in memory. |

### Actors and trust boundaries

Actors: external client; unauthenticated network scanner; stolen-token holder;
reverse proxy operator; host administrator or process introspector; insider
with backup access; supply-chain attacker.

Boundaries: (1) client to TLS proxy, untrusted network; (2) proxy to service on
loopback, trusted only when co-located; (3) service to seed source; (4) service
to index directory and backups; (5) build/release to artifact verification;
(6) service response to relying party, where the certificate is an unauthenticated
claim.

### STRIDE-style analysis

| Class | Threat | Mitigation | Residual |
| --- | --- | --- | --- |
| Spoofing | Forged bearer token on a non-loopback bind | Token required for non-loopback; empty and whitespace-only tokens rejected; constant-time compare; TLS and client auth at proxy | Token in env, process args, or proxy logs is replayable; service cannot authenticate the proxy beyond the token |
| Tampering | Modified index files | Load verifies schema, seed commitment, and BLAKE3 `index_id`; saves are fsynced and atomically swapped | `index_id` is integrity, not authenticity: a writer can recompute it. Index forgery is undetectable without an externally signed manifest |
| Tampering | Edited certificate | `pqr-guard verify --certificates` recomputes the claim | Certificates are unsigned in v0.1; verification does not attest to assumptions or producer identity, so a self-consistent forgery passes |
| Repudiation | No proof of which instance produced a certificate or who reloaded an index | Structured logs for requests and reloads; external log shipping | Logs are not signed and can be altered by a host admin |
| Information disclosure | Seed exposed through `PQR_SEED_ENV` | Prefer seed file with `0600` or audited KMS command | Env values remain visible to host processes and dumps; no in-process protection |
| Information disclosure | Tokens, vectors, or bodies captured in logs | Service logs only method, path, status, elapsed; proxy must apply the same hygiene | Proxy log configuration is outside this service |
| Denial of service | Oversized bodies, extreme `k`/`nprobe`, concurrency exhaustion | Body limit, target bounds, hard concurrency cap of 64, query timeout, proxy rate limit | Requests beyond `PQR_MAX_CONCURRENCY` queue in-process; bound the queue at the proxy |
| Elevation of privilege | A query client reaches `/admin/reload` or `/metrics` | Bearer token on all protected routes | One token grants every role; path-level ACL must be enforced at the proxy |
| Supply chain | Tampered binary or dependency in production | Release checksums, Sigstore bundle verification, attestations, and SBOM from the workflow README | Protection fails if operators skip verification; signing identity lives outside the service |

### Residual risks (explicit)

- Certificates carry **no signature**. They are self-consistent claims, not
  authenticated attestations, and v0.1 has no producer identity.
- The index checksum detects accidental corruption, not malicious **index
  forgery**: anyone who can write the directory can recompute `index_id`.
- There is **no built-in TLS**. Without an external proxy, tokens and query
  vectors cross the network in plaintext.
- Environment-based seed custody exposes the seed to local inspection and
  crash artifacts. Use file or KMS custody when that matters.
- The **external proxy owns** TLS termination, client authentication, token
  injection, rate limiting, path ACLs, and log hygiene. A misconfigured proxy
  voids the service's protections.
- Revocation is limited to replacing the single configured token; no
  per-client credentials or revocation list exist.

## Backup and recovery

### What to back up

- The whole index directory as one consistent unit: `manifest.json`,
  `vectors.f32`, `centroids.f32`, `assign.u32`, `norms.f32`, `codes.u64`,
  `coeffs.f64`, `coeffc.f64`, `order.u32`, `starts.u32`. The manifest's
  `index_id` identifies the set and is verified on load.
- The seed, **separately** from the index backup, under secret-manager ACLs.
  Never place raw seed bytes in the same archive or ticket as the index.
- Configuration: the non-secret flags/env template, proxy config, and a
  reference to the secret holding the token. Never back up the token value in
  plaintext.

`PqrIndex::save` writes all ten files into a fresh sibling staging directory,
fsyncs each file and the directory, then atomically swaps the directory into
place. A failed swap restores the previous index, which stays loadable until
the swap succeeds; `index_id` is unchanged. Back up a completed directory, not
one being written by a build, reload, or `pqr-guard audit`. Load caps `dim` at
4096, `n` and `partitions` at 20000000, stages at 64, each data file at 1 GiB,
and the manifest at 1 MiB, so an oversized backup is rejected before
allocation. Note that `audit` reencodes the index under its final rotation and
changes `index_id`.

### Checksums and restore test

Record an external SHA-256 manifest for the index directory alongside the
built-in `index_id`, and store it where the index writer cannot alter it. On
restore, compare the recorded checksum and `index_id`, then start the service
against the restored directory with the correct seed and confirm `/readyz`
returns 200.

Run a restore test at least quarterly and after every restore procedure change:
restore into a staging path, issue a known certified query, run
`pqr-guard verify --certificates` on the output, and compare the certificate's
`index_id` to the restored manifest. Record the elapsed time as the measured
RTO. A passing verification is self-consistency only; it does not authenticate
the backup.

### RPO and RTO guidance

v0.1 indexes are static: there is no insert, delete, or merge API. Index RPO is
therefore the interval between retained builds; keep the previous build until
the new one is proven. Seed RPO is the secret manager's replication delay. As
starting targets, plan for a 15 minute RTO from a local snapshot and 1 hour from
cold storage, then replace these numbers with measured restore times.

### Restore and index reload

Point `PQR_INDEX` at the restored directory and call `POST /admin/reload` or
send `SIGHUP`. Expect a brief 503 window while readiness is false. After the
reload, check `/readyz`, sample `/v1/query`, and verify the certificate's
`index_id`. Keep the prior directory in place until traffic and verification
look normal; roll back by pointing back to it and reloading again.

## Observability

### Metrics

`GET /metrics` renders Prometheus text with these names:

- `pqr_http_requests_total{route,code}` — counter; `route` is one of
  `/healthz`, `/readyz`, `/metrics`, `/v1/query`, `/admin/reload`, or `other`;
  `code` is `2xx`, `4xx`, or `5xx`.
- `pqr_http_errors_total{reason}` — counter; `reason` is `auth`, `timeout`,
  `client`, or `server`. Only non-2xx responses increment it.
- `pqr_query_duration_seconds` — histogram of `/v1/query` wall time with
  buckets `0.001`, `0.005`, `0.01`, `0.05`, `0.1`, `0.5`, `1`, `+Inf`, plus
  `_sum` and `_count`.

Useful expressions:

```promql
rate(pqr_http_requests_total{route="/v1/query",code="5xx"}[5m])
rate(pqr_http_errors_total{reason="timeout"}[5m])
histogram_quantile(0.95, sum by (le) (rate(pqr_query_duration_seconds_bucket[5m])))
```

### Logs

`tracing-subscriber` emits JSON. Request records carry `timestamp`, `level`,
`target`, message `"request"`, and fields `method`, `path`, `status`,
`elapsed_ms`. Startup records `bind` and `index`; reload records use
`event="signal_reload"` or the message `index reload rejected` with an `error`
field. Set `RUST_LOG` to change the filter. Bodies, seeds, tokens, and query
vectors are never logged by this service; verify the proxy does the same.

### Suggested alerts and SLOs

Starting thresholds; tune to the local baseline from the load test.

| Signal | Alert | Action |
| --- | --- | --- |
| `/readyz` failing | 2 minutes | Page; remove instance, inspect reload logs |
| Query 5xx rate | > 1% over 5 minutes | Page; check index, seed, and dependencies |
| Timeout errors | > 1% over 5 minutes | Page; reduce concurrency or raise timeout within proxy deadline |
| Auth errors | > 10/min or sharp spike | Ticket; check token rotation and proxy injection |
| p95 query latency | > 500 ms or > 0.8 x timeout over 10 minutes | Ticket; profile and re-run load test |
| Query request rate | zero for 5 minutes in business hours | Ticket; check proxy and DNS |

Starting SLOs: 99.9% monthly readiness availability; at least 99.9% of accepted
certified queries return a certificate (excluding intentional degraded
requests); p95 at or below 500 ms at the agreed concurrency; zero logged seeds,
tokens, or vectors; 100% of release artifacts verified before deployment.

Bodies are capped before parsing, query work runs on the blocking pool under a
wall-clock timeout that includes blocking-pool queue time, and concurrency is
capped at 64 requests to stay below the blocking pool. Logs are JSON with
method, path, status, and elapsed time; bodies, seeds, tokens, and query vectors
are never logged. Set `RUST_LOG` to change the tracing filter.

## Incident response

### Detection

Use the alerts above plus these signals: a burst of degraded responses, a
changed `index_id`, certificate verification failures, an unexpected reload,
409 responses from `/admin/reload`, warnings about a busy `SIGHUP` or a
recovered poisoned lock, 401 spikes, TLS errors at the proxy, or a seed/token
appearing where it should not.

### Triage

Check `/readyz`, metrics, and logs (redacting any secret-bearing fields before
sharing). Confirm the running `index_id` and seed commitment against the
recorded manifest. Determine whether the fault is one instance or the whole
pool, and whether the token, seed, index, or proxy is involved. Assign a
severity and an incident owner.

### Containment

Stop traffic to affected instances at the proxy. Revoke a token by rotating it
at the proxy and restarting the service with a different value. If seed
exposure is suspected, treat every certificate produced with that seed as
untrusted: build a new index from a new seed, deploy it via the staged reload
path, and revoke the old token. Preserve logs and a read-only copy of the
suspect index directory before wiping anything.

### Eradication

Remove tampered index directories and untrusted builds. Rotate every credential
that may be exposed, including proxy secrets and the seed. Rebuild from
verified release artifacts using the workflow README checks. Fix the proxy
configuration that allowed the exposure.

### Recovery

Restore the last known-good index and seed, reload, confirm `/readyz`, run a
sample certified query, and verify its certificate and `index_id`. Ramp traffic
back through the proxy and watch 5xx, timeout, and latency alerts before
closing the incident.

### Evidence and postmortem

Copy request logs, proxy access logs, metrics snapshots, the suspect index
directory, and the config with tokens redacted into restricted storage. Record
SHA-256 hashes and who collected each item; do not include raw seed bytes.
Write a postmortem with a timeline, root cause, contributing factors, detection
gaps, and corrective actions with owners and dates. Update alerts and this
runbook with the lessons.

## Dependencies

- `axum` 0.8 — router, extractors, body limit, and graceful HTTP serving.
- `tokio` 1 — async runtime, TCP listener, signals, timeouts, and blocking query tasks.
- `serde` / `serde_json` — JSON request/response shapes and response encoding.
- `thiserror` — typed startup/index errors.
- `tracing` / `tracing-subscriber` — structured JSON logs and `RUST_LOG` filtering.
- `tower` / `tower-http` — concurrency limit and request body limit layers.
- `clap` — flag parsing plus environment-variable defaults.
- `pqr-core` — index loading, query execution, and certificate types.

Development-only: `tempfile` for generated index fixtures.
