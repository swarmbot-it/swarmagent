# swarmagent

Lightweight [Swarmbot](https://github.com/dcniko/swarmbot) Docker agent written in Rust. It streams Docker events and periodic host/container stats to the Swarmbot application, and exposes a small HTTP API.

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
| `EVENT_ENDPOINT` | `http://app:8080/events` | Swarmbot events ingest URL |
| `HEALTH_CHECK_ENDPOINT` | `http://app:8080/version` | URL polled every 5s until Swarmbot is up |
| `DEBUG_EVENT` | `false` | Log outbound Docker events (debug) |
| `DEBUG_STATS` | `false` | Log outbound stats JSON (debug) |
| `STATS_MAX_CONCURRENCY` | `32` | Max parallel `docker stats` calls per tick |
| `LOGS_MAX_BYTES` | `4194304` | Max decoded log bytes for `GET /logs/...` (413 if exceeded) |

## HTTP API

- `GET /` - JSON runtime configuration.
- `GET /logs/{container}?since=` — Container logs as a JSON string (stdout+stderr, timestamps). `since` is Unix seconds or RFC3339.

## Resource profile

Release builds use **LTO**, **single codegen unit**, **strip**, and **`panic = "abort"`** (see `Cargo.toml`). The Docker image is **`scratch`** with a **musl**-linked static binary. A single `reqwest::Client` is reused for Swarmbot HTTP calls.

## License

MIT — see [LICENSE](LICENSE).
