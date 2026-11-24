# Deploying pqr-serve

`pqr-serve` is a stateless HTTP frontend over a read-only index directory.
It serves plaintext HTTP. A reverse proxy terminates TLS; the service never
handles certificates or public traffic directly.

## Files

| Path | Purpose |
| --- | --- |
| `Dockerfile` | Multi-stage image: Rust builder, slim non-root runtime. |
| `docker-compose.yml` | Container run: loopback port, read-only index, seed secret. |
| `pqr-serve.service` | systemd unit with hardening and SIGHUP reload. |
| `smoke.sh` | End-to-end health, ready, query, and metrics check. |
| `../.dockerignore` | Keeps target, fixtures, docs, VCS data, and seed-like files out of the build context. |

The image bakes in no seed, index, or token. All three are supplied at run time.

## Build

From the repository root:

```sh
docker build -f deploy/Dockerfile -t pqr-serve:local .
```

The builder runs `cargo build --locked --release -p pqr-service`. Base images are
pinned by digest. Refresh digests deliberately when upgrading the toolchain.
A host binary build is also supported:

```sh
cargo build --locked --release -p pqr-service -p pqr-cli
```

## Seed

The seed is 32 raw bytes or 64 hex characters. It is never committed.

```sh
install -d -m 0700 /etc/pqr
openssl rand -hex 32 > /etc/pqr/seed.hex
chmod 0600 /etc/pqr/seed.hex
chown pqr:pqr /etc/pqr/seed.hex
```

`Seed::from_file` rejects symlinks, group/other-readable files, and files over
64 bytes. Alternatives are `--seed-env PQR_SEED_NAME` (value held in a process
environment variable) and `--seed-command` (a command printing the seed to
stdout). Prefer a file or secret mount: environment values leak through process
inspection and crash dumps. Rotate a seed only with a matching index rebuild;
changing it invalidates every certificate issued from the old index.

## Index

Build the index with the same seed the service will use:

```sh
cargo run --locked --release -p pqr-cli -- build \
  --vectors vectors.npy --seed-file /etc/pqr/seed.hex \
  --out /var/lib/pqr/index
```

The manifest records a seed commitment, so a mismatched seed fails startup.
Mount or expose the index directory read-only. For reloads, keep versioned
directories and a stable symlink at `PQR_INDEX`, then swap the symlink.

## Run with Compose

Set paths and token in the environment; never write a token into
`docker-compose.yml` or a tracked file:

```sh
export PQR_INDEX_DIR=/var/lib/pqr/index
export PQR_SEED_FILE=/etc/pqr/seed.hex
export PQR_API_TOKEN="$(openssl rand -hex 32)"
docker compose -f deploy/docker-compose.yml up -d
```

Compose publishes `127.0.0.1:8080` only, mounts the index `ro`, mounts the seed
as a secret at `/run/secrets/pqr_seed`, restarts on failure, sets CPU/memory
limits, and uses a `/healthz` healthcheck. Compose ignores secret `uid` and
`mode` outside Swarm, so the host seed file must be mode `0600` and owned by
uid `10001`; the service rejects group/other-readable seeds. The token is
required because the container binds `0.0.0.0:8080`.

## Run with systemd

Install the binary, unit, and directories as root:

```sh
install -m 0755 target/release/pqr-serve /usr/local/bin/pqr-serve
install -m 0644 deploy/pqr-serve.service /etc/systemd/system/pqr-serve.service
useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin pqr
install -d -m 0700 -o pqr -g pqr /etc/pqr
install -d -m 0550 -o pqr -g pqr /var/lib/pqr/index
```

Write `/etc/pqr/pqr-serve.env` with mode `0600`, owner `pqr:pqr`, using a token
from the secret store:

```sh
PQR_BIND=127.0.0.1:8080
PQR_INDEX=/var/lib/pqr/index
PQR_SEED_FILE=/etc/pqr/seed.hex
PQR_API_TOKEN=<token-from-secret-store>
RUST_LOG=info
```

Then enable the unit:

```sh
systemctl daemon-reload
systemctl enable --now pqr-serve
```

The unit uses `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`,
`NoNewPrivileges`, `RestrictAddressFamilies`, `MemoryDenyWriteExecute`, and
`ReadOnlyPaths` for the index. Logs go to the journal as JSON.

## Reverse proxy and TLS boundary

Terminate TLS at nginx, Caddy, or an ingress in front of `127.0.0.1:8080`.
Forward the `Authorization` header to the service; do not let the proxy add,
strip, or log the token. Keep port 8080 off all public interfaces and firewall
it from untrusted hosts. `/healthz` and `/readyz` are unauthenticated by design;
restrict them at the proxy if they must not be reachable off-box. `/metrics` and
`/admin/reload` require the bearer token. Set HSTS and redirect HTTP to HTTPS at
the proxy. A loopback bind does not require `PQR_API_TOKEN`; set one anyway for
defence in depth.

## Health and readiness

```sh
curl -fsS http://127.0.0.1:8080/healthz
curl -fsS http://127.0.0.1:8080/readyz
```

`/healthz` returns `ok` while the process is up. `/readyz` returns `ready` when
an index is loaded and `not ready` with HTTP 503 while a reload is in progress
or after a failed reload. Point proxy readiness checks at `/readyz`, not
`/healthz`.

## Graceful reload

SIGHUP reloads the index from `PQR_INDEX` in a blocking worker without dropping
connections:

```sh
systemctl reload pqr-serve
docker compose -f deploy/docker-compose.yml kill -s HUP pqr-serve
```

The service sets readiness false while loading. A failed load leaves readiness
false until the index directory is fixed and another SIGHUP succeeds. Keep the
seed unchanged across reloads; write a new index to a fresh directory, swap the
stable symlink, then signal. SIGTERM drains in-flight requests; allow at least
the configured stop grace period.

## Backup and restore

Back up the index directory, the seed file, and the environment file to separate
encrypted locations. An index backup without its seed cannot be restored, and a
token must be restored separately from the secret store. To restore, stop the
service, restore the index and seed with original ownership and permissions,
start the service, and check `/readyz` and one query. Test restores before
relying on them.

## Observability

`GET /metrics` needs `Authorization: Bearer <token>` and returns Prometheus text
with `pqr_http_requests_total`, `pqr_http_errors_total`, and
`pqr_query_duration_seconds_*`. A scrape target can set
`metrics_path: /metrics` and an authorization header from the secret store.
Application logs are newline-delimited JSON on stdout; `RUST_LOG` controls
filtering (for example `info,pqr_service=debug`). Never ship logs containing
tokens or raw seeds.

## Upgrade and rollback

1. Build a new image or binary and run `deploy/smoke.sh` first.
2. Record the current image digest or binary checksum and index snapshot.
3. Replace the image or binary, restart, then check `/readyz`, one query, and metrics.
4. Roll back by redeploying the recorded digest or binary and restoring the old index snapshot.

A seed change requires rebuilding the index; rollback must restore both.

## Verify the image has no seed

```sh
docker run --rm --entrypoint sh pqr-serve:local -c \
  'test ! -e /run/secrets/pqr_seed && test -z "${PQR_SEED_FILE:-}" && test -z "${PQR_SEED_ENV:-}" && test -z "${PQR_SEED_COMMAND:-}"'
docker inspect --format '{{json .Config.Env}}' pqr-serve:local
docker history --no-trunc pqr-serve:local | grep -i seed
```

The last command must print nothing: no seed file, env value, or build layer.
The `.dockerignore` also blocks `*.hex`, `*.pem`, `*.key`, and `.env*` files from
the build context.

## Smoke test

```sh
deploy/smoke.sh
deploy/smoke.sh --binary /path/to/pqr-serve
```

The script builds the image when Docker is usable, otherwise builds or accepts
a local service binary. It generates a temporary seed and index, picks a free
loopback port, asserts health, readiness, a certified query response, metrics,
and rejection of unauthenticated metrics, then removes the container, process,
image, and temporary files. Requires `python3` and `curl`.
