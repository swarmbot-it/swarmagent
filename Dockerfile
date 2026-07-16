# syntax=docker/dockerfile:1

FROM rust:1.88-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rustup target add x86_64-unknown-linux-musl \
    && cargo build --release --target x86_64-unknown-linux-musl

FROM scratch
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/swarmagent /swarmagent
# Push-only agent: no ports are exposed. In Kubernetes mode the agent talks
# TLS to the API server, so the CA bundle location must exist for rustls.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
# Root required to read the mounted Docker socket (mode 0660 on Swarm nodes).
# In Kubernetes mode run as non-root instead (set runAsUser in the DaemonSet).
USER 0:0
ENTRYPOINT ["/swarmagent"]
