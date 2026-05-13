# Railway source-build image for the Portunus server.
#
# This differs from deploy/docker/Dockerfile.server, which expects release
# binaries to be pre-staged under docker-bin/. Railway builds directly from
# the Git repository, so it needs the Web UI and Rust build chain in-image.

FROM node:20-bookworm AS webui-builder
WORKDIR /app/webui
COPY webui/package.json webui/pnpm-lock.yaml ./
RUN corepack enable && corepack prepare pnpm@9.12.3 --activate
RUN pnpm install --frozen-lockfile
COPY webui/ ./
# `pnpm build` also runs size-limit, which shells out to headless Chrome.
# That belongs in CI; the production image only needs typed Vite assets.
RUN pnpm exec tsc -b && pnpm exec vite build

FROM rust:1.88-bookworm AS rust-builder
WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY proto ./proto
COPY crates ./crates
COPY --from=webui-builder /app/webui/dist ./webui/dist
ENV CARGO_INCREMENTAL=0
RUN cargo build --release -p portunus-server

FROM debian:12-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/portunus
COPY --from=rust-builder /app/target/release/portunus-server /usr/local/bin/portunus-server
COPY deploy/railway/start-server.sh /usr/local/bin/portunus-railway-start
RUN chmod 0755 /usr/local/bin/portunus-server /usr/local/bin/portunus-railway-start
EXPOSE 7443
CMD ["/usr/local/bin/portunus-railway-start"]
