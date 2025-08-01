FROM rust:1.85 AS builder

WORKDIR /app
COPY . .
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12:nonroot AS runner

WORKDIR /home/nonroot
COPY --from=builder /app/target/release/agent ./app

ARG SERVICE_VERSION=unknown
ENV SERVICE_VERSION=$SERVICE_VERSION

EXPOSE 8080
CMD ["./app"]
