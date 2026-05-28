# syntax=docker/dockerfile:1.7
#
# Reference Dockerfile for portunus-standalone — not the published image.
# Build from the repo root:
#   docker build -t portunus-standalone:dev \
#     -f crates/portunus-standalone/contrib/Dockerfile .
#
# Runtime image is distroless `nonroot` (UID 65532). Binding privileged
# ports (<1024) inside the container requires the host to grant the
# NET_BIND_SERVICE capability — see contrib/docker-compose.yml or pass
# `--cap-add NET_BIND_SERVICE` to `docker run`.

FROM rust:1.88-bookworm AS builder
WORKDIR /usr/src/portunus
# Cache dependencies via the full workspace first; subsequent rebuilds
# only re-link portunus-standalone when its sources change.
COPY . .
RUN cargo build --release -p portunus-standalone \
 && strip target/release/portunus-standalone \
 && mkdir -p /tmp/run-portunus \
 && chown 65532:65532 /tmp/run-portunus \
 && chmod 0755 /tmp/run-portunus

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /usr/src/portunus/target/release/portunus-standalone \
     /usr/local/bin/portunus-standalone
COPY --from=builder --chown=65532:65532 /tmp/run-portunus /run/portunus
USER nonroot
ENTRYPOINT ["/usr/local/bin/portunus-standalone"]
CMD ["--config", "/etc/portunus/standalone.toml"]
