FROM docker.io/library/rust:1.100-slim-bookworm@sha256:ac965c2299127add362cdcf165f19516a0ca90d893bf7bd04cf1a44de39cfca7 AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release -p pqr-service

FROM docker.io/library/debian:bookworm-slim@sha256:e67eeead736ff7bbe258ef088bff683635c561b1fcfefad0806c1df4ab49ce0e AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates wget \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 pqr \
 && useradd --system --uid 10001 --gid pqr --home-dir /nonexistent --shell /usr/sbin/nologin pqr \
 && mkdir -p /var/lib/pqr/index \
 && chown -R pqr:pqr /var/lib/pqr
COPY --from=builder /src/target/release/pqr-serve /usr/local/bin/pqr-serve
USER pqr:pqr
ENV PQR_INDEX=/var/lib/pqr/index \
    PQR_BIND=0.0.0.0:8080 \
    RUST_LOG=info
EXPOSE 8080
VOLUME ["/var/lib/pqr"]
HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3 CMD ["wget", "-q", "-O", "/dev/null", "http://127.0.0.1:8080/healthz"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/pqr-serve"]
