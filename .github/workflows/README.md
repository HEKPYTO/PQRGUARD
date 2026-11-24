# CI and release workflows

## Permissions and pinning

All workflows use `permissions: contents: read` by default. Only the release
`package` job escalates to `attestations: write` and `id-token: write` for
build provenance and keyless signing. No secrets are used. Every `uses` step
is pinned to a full commit SHA; the trailing comment names the release tag.
Checkouts set `persist-credentials: false`, and all Cargo build/test commands
use `--locked`.

## Workflows

- `ci.yml` — format, clippy, tests, `cargo deny check`, `cargo audit`, two
  CycloneDX binary SBOMs, and a fast `bench-syntax` job (`bash -n`, Python
  compile, CLI `--help`). Publishes artifact `sbom` with
  `sbom-pqrguard.cdx.json`, `sbom-pqr-serve.cdx.json`, and `Cargo.lock`.
  `cargo-cyclonedx --describe binaries` emits one SBOM per binary in the
  workspace, covering both shipped binaries.
- `nightly.yml` — scheduled certified audit with a generated fixture.
- `benchmark.yml` — nightly and manual 1M contract. Caches SIFT fixtures,
  runs `benchmarks/fetch_sift.sh`, bootstraps the pinned HNSW cache on a cold
  cache, calls `benchmarks/run_1m.sh` (`--reuse-index` when an index exists),
  uploads `bench-report`, and fails on any gate breach.
- `release.yml` — runs on tags `v*` or `workflow_dispatch`. Builds `pqrguard`
  and `pqr-serve` for `x86_64-unknown-linux-gnu` (native) and
  `aarch64-unknown-linux-gnu` (via `cross`, which needs Docker). Each build
  publishes `pqr-binaries-<target>` with both binaries and one SBOM per binary.
  The `package` job downloads both artifacts and publishes `release-assets`
  containing four binaries, four SBOMs, `Cargo.lock`, `SHA256SUMS`, and one
  Sigstore bundle (`*.sigstore.json`) per file. GitHub build provenance
  attestations are attached to the workflow run, not to a GitHub Release.

## Verify a release artifact

Download `release-assets` from the workflow run and unpack it into `dist/`.
Replace `HEKPYTO/PQRGUARD` with the repository if using a fork.

1. Checksums:

   ```sh
   cd dist
   sha256sum -c SHA256SUMS
   ```

2. Keyless Sigstore signatures (bundles contain the signature, certificate,
   and transparency-log proof):

   ```sh
   for file in pqrguard-* pqr-serve-* sbom-*.cdx.json Cargo.lock SHA256SUMS; do
     cosign verify-blob \
       --bundle "$file.sigstore.json" \
       --certificate-oidc-issuer https://token.actions.githubusercontent.com \
       --certificate-identity-regexp '^https://github\.com/HEKPYTO/PQRGUARD/\.github/workflows/release\.yml@refs/(tags|heads)/' \
       "$file"
   done
   ```

3. GitHub build provenance:

   ```sh
   for file in pqrguard-x86_64-unknown-linux-gnu pqr-serve-x86_64-unknown-linux-gnu \
     pqrguard-aarch64-unknown-linux-gnu pqr-serve-aarch64-unknown-linux-gnu; do
     gh attestation verify "dist/$file" \
       --repo HEKPYTO/PQRGUARD \
       --signer-workflow HEKPYTO/PQRGUARD/.github/workflows/release.yml
   done

4. SBOM contents:

   ```sh
   jq -r '.bomFormat, .specVersion, .metadata.component.name' dist/sbom-*.cdx.json
   ```

## Hosted-runner requirements

`cargo install` steps compile pinned tool versions and need network access to
crates.io. `cargo deny check` and `cargo audit` fetch the RustSec advisory
database. GitHub artifact attestations are free on public repositories; a
private repository needs GitHub Advanced Security. `cross` needs Docker,
available on `ubuntu-latest`.

`benchmark.yml` needs outbound FTP access to `ftp.irisa.fr` for
`fetch_sift.sh`, enough RAM for the 1M graph (reference machine: 24 GB), and a
cache hit or deterministic regeneration of the pinned HNSW graph. A cold-cache
graph generated on Linux must match the pinned checksum; if it does not, seed
the Actions cache from the verified local fixture before relying on this job.
