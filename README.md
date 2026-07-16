# swarmagent

Lightweight [Swarmboty](https://github.com/dcniko/swarmboty) monitoring agent written in Rust. It runs on **Docker Swarm** (or a standalone Docker Engine) and on **Kubernetes / k3s**, auto-detecting the orchestrator at startup. The agent is **push-only**: it exposes no ports and only sends events and periodic host/container stats to the Swarmboty application.

## Requirements

- Rust **1.86+** (CI uses 1.88; see `rust-version` in `Cargo.toml`)
- Docker mode: Docker Engine with API reachable via default socket or `DOCKER_HOST`
- Kubernetes mode: any conformant cluster (tested against k3s); the agent runs as a DaemonSet pod with a ServiceAccount

## Orchestrator auto-detection

At startup the agent resolves its mode (`AGENT_MODE=auto`, the default):

1. **Kubernetes** — when the in-cluster ServiceAccount token is mounted and `KUBERNETES_SERVICE_HOST` is set (always true inside a pod).
2. **Docker** — otherwise, by connecting to the local Docker socket.

Set `AGENT_MODE=docker` or `AGENT_MODE=kubernetes` to force a mode.

| | Docker / Swarm | Kubernetes / k3s |
|---|---|---|
| Node identity (`id`) | Swarm node ID (empty outside Swarm) | node name |
| Host metrics | `sysinfo` (CPU/RAM/disk) | kubelet Summary API (no `/proc` mounts needed) |
| Container metrics | `docker stats` per container | Summary API per pod/container |
| Events | `docker events` stream | pod watch scoped to this node (`start`/`die`/`oom`/`destroy`) |
| Container `id` format | Docker container ID | `{namespace}/{pod}/{container}` |
| Extra payload fields | — | `namespace`, `pod`, `workload`, `workloadKind` per container |

Every payload carries `orchestrator: "swarm" \| "kubernetes"`.

## Run (Docker)

```bash
docker run -d \
  --name swarmagent \
  -v /var/run/docker.sock:/var/run/docker.sock:ro \
  -e SW4RM_BOT_URL=http://app:8080 \
  ghcr.io/dcniko/swarmagent:latest
```

No ports are published — the agent only makes outbound HTTP calls to Swarmboty.

Build the static image locally:

```bash
docker build -t swarmagent:dev .
```

## Run (Kubernetes / k3s)

Deploy the DaemonSet with RBAC (one agent per node):

```bash
kubectl apply -f deploy/k8s/swarmagent.yaml
```

Edit `SW4RM_BOT_URL` in the manifest to point at your Swarmboty API. The manifest grants the ServiceAccount read access to `nodes`, `nodes/stats`, `nodes/proxy`, and `pods`, and passes `NODE_NAME`/`NODE_IP` via the Downward API. In Kubernetes mode the agent does not need the Docker socket or root.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `AGENT_MODE` | `auto` | `auto`, `docker`, or `kubernetes` |
| `SW4RM_BOT_URL` (alias `SWARMBOTY_URL`) | `http://app:8080` | Swarmboty base URL; `/events` and `/version` are derived from it |
| `EVENT_ENDPOINT` | `<base>/events` | Override for the events ingest URL |
| `HEALTH_CHECK_ENDPOINT` | `<base>/version` | URL polled every 5s until Swarmboty is up |
| `STATS_FREQUENCY` | `30` | Seconds between stats payloads (first sample is sent immediately on startup) |
| `STATS_MAX_CONCURRENCY` | `32` | Docker mode: max parallel `docker stats` calls per tick |
| `NODE_NAME` | — | Kubernetes mode (**required**): node name via Downward API (`fieldRef: spec.nodeName`) |
| `NODE_IP` | — | Kubernetes mode: host IP for direct kubelet access (`fieldRef: status.hostIP`); resolved from the Node object when unset |
| `AGENT_KUBELET_MODE` | `direct` | Kubernetes mode: `direct` (kubelet :10250, falls back to proxy) or `proxy` (always via API server) |
| `AGENT_KUBELET_INSECURE_TLS` | `true` | Kubernetes mode: skip TLS verification for direct kubelet calls (k3s serves a self-signed cert) |
| `DEBUG_EVENT` | `false` | Log outbound events (debug) |
| `DEBUG_STATS` | `false` | Log outbound stats JSON (debug) |

## Data contract

All payloads are POSTed to `EVENT_ENDPOINT` in the envelope
`{"type": "stats" | "event", "message": …}`. The `stats` message is the
`Status` model (see `src/models.rs`); in Kubernetes mode containers carry
additional optional fields (`namespace`, `pod`, `workload`, `workloadKind`)
that are omitted in Docker mode, keeping Swarm payloads byte-compatible with
previous agent versions.

## Resource profile

Release builds use **LTO**, **single codegen unit**, **strip**, and **`panic = "abort"`** (see `Cargo.toml`). The Docker image is **`scratch`** with a **musl**-linked static binary. A single `reqwest::Client` is reused for Swarmboty HTTP calls; Kubernetes mode uses the same `reqwest` stack (no heavyweight Kubernetes client dependency).

## Development

CI (see [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs:

```bash
cargo fmt --all -- --check
cargo clippy --release -- -D warnings
cargo test --release
```

Implementation plan and architecture notes: [`docs/PLAN-multi-orchestrator.md`](docs/PLAN-multi-orchestrator.md).

### Tests after a local image build

The runtime image (`swarmagent:dev` / `latest`) is based on **`scratch`** and contains only the static binary — no shell, Rust toolchain, or test runner. Run checks in the **`builder`** stage of the same `Dockerfile` (layers are reused from `docker build`):

```bash
docker build -t swarmagent:dev .

docker build --target builder -t swarmagent:builder .
docker run --rm swarmagent:builder cargo fmt --all -- --check
docker run --rm swarmagent:builder cargo clippy --release -- -D warnings
docker run --rm swarmagent:builder cargo test --release
```

On a machine without a local Rust install, you can run the same commands in one shot (mount the repo; no prior image build required):

```bash
docker run --rm -v "${PWD}:/app" -w /app rust:1.88-bookworm bash -lc \
  'rustup component add clippy rustfmt 2>/dev/null; \
   cargo fmt --all -- --check && \
   cargo clippy --release -- -D warnings && \
   cargo test --release'
```

## License

MIT — see [LICENSE](LICENSE).
