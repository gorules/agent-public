# Dependencies and the precompiled tsgo module share a layer that depends only
# on the manifests and build.rs. Editing src/ reuses it, so a source-only
# change no longer recompiles wasmtime or spends ~26 CPU-seconds precompiling
# the module again.
FROM rust:1.96 AS deps

WORKDIR /app
ENV TSGO_CWASM_CACHE=/app/.tsgo-cache

COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release \
    && rm -rf src target/release/.fingerprint/agent-*

FROM rust:1.96 AS builder

WORKDIR /app
ENV TSGO_CWASM_CACHE=/app/.tsgo-cache

COPY --from=deps /app/target target
COPY --from=deps /app/.tsgo-cache .tsgo-cache
COPY --from=deps /usr/local/cargo/registry /usr/local/cargo/registry

COPY . .
RUN cargo build --release

FROM gcr.io/distroless/cc-debian13:nonroot AS runner

WORKDIR /home/nonroot
COPY --from=builder /app/target/release/agent ./app

ARG SERVICE_VERSION=unknown
ENV SERVICE_VERSION=$SERVICE_VERSION

EXPOSE 8080
CMD ["./app"]
