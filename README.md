# swarmagent

Lightweight [Swarmboty](https://github.com/dcniko/swarmboty) Docker agent written in Rust. It streams Docker events and periodic host/container stats to the Swarmboty application, and exposes a small HTTP API.

## Requirements

- Rust **1.86+** (CI uses 1.88; see `rust-version` in `Cargo.toml`)
- Linux agent container: Docker Engine with API reachable via default socket or `DOCKER_HOST`

## Run (Docker)

```bash
docker run -d \
  --name swarmagent \
  -p 8080:8080 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -e EVENT_ENDPOINT=http://app:8080/events \
  -e HEALTH_CHECK_ENDPOINT=http://app:8080/version \
  ghcr.io/dcniko/swarmagent:latest
```

Build the static image locally:

```bash
docker build -t swarmagent:dev .
```

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `STATS_FREQUENCY` | `30` | Seconds between stats payloads |
| `EVENT_ENDPOINT` | `http://app:8080/events` | Swarmboty events ingest URL |
| `HEALTH_CHECK_ENDPOINT` | `http://app:8080/version` | URL polled every 5s until Swarmboty is up |
| `DEBUG_EVENT` | `false` | Log outbound Docker events (debug) |
| `DEBUG_STATS` | `false` | Log outbound stats JSON (debug) |
| `STATS_MAX_CONCURRENCY` | `32` | Max parallel `docker stats` calls per tick |
| `LOGS_MAX_BYTES` | `4194304` | Max decoded log bytes for `GET /logs/...` (413 if exceeded) |

## HTTP API

- `GET /` - JSON runtime configuration.
- `GET /logs/{container}?since=` — Container logs as a JSON string (stdout+stderr, timestamps). `since` is Unix seconds or RFC3339.

## Resource profile

Release builds use **LTO**, **single codegen unit**, **strip**, and **`panic = "abort"`** (see `Cargo.toml`). The Docker image is **`scratch`** with a **musl**-linked static binary. A single `reqwest::Client` is reused for Swarmboty HTTP calls.

## Development

CI (see [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs:

```bash
cargo fmt --all -- --check
cargo clippy --release -- -D warnings
cargo test --release
```

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
