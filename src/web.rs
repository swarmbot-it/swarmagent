//! Axum HTTP server — diagnostic endpoints exposed by the agent.
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | `GET` | `/` | Agent configuration summary ([`info`]). |
//! | `GET` | `/version` | Raw `docker version` output ([`version`]). |
//! | `GET` | `/logs/:container` | Recent container logs ([`logs`]). |
//! | `GET` | `/inspect/:container` | Full `docker inspect` output ([`inspect`]). |
//!
//! All routes are covered by the [`access_log`] middleware which emits a
//! structured `DEBUG` log line for every request.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::header::{REFERER, USER_AGENT};
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use bollard::container::{InspectContainerOptions, LogsOptions};
use bollard::Docker;
use bytes::BytesMut;
use futures_util::StreamExt;
use serde::Deserialize;
use tracing::debug;

use crate::config::Config;

/// Shared state injected into every handler via [`axum::extract::State`].
#[derive(Clone)]
pub struct AppState {
	/// Bollard Docker client (version-negotiated).
	pub docker: Docker,
	/// Agent configuration.
	pub config: Arc<Config>,
}

/// Builds the router with all routes and the access-log middleware attached.
pub fn router(state: AppState) -> Router {
	Router::new()
		.route("/", get(info))
		.route("/version", get(version))
		.route("/logs/:container", get(logs))
		.route("/inspect/:container", get(inspect))
		.layer(middleware::from_fn(access_log))
		.with_state(state)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /` — returns the agent's active configuration as JSON.
async fn info(State(state): State<AppState>) -> Json<crate::config::InfoResponse> {
	Json(state.config.to_info_json())
}

/// `GET /version` — returns the raw output of `docker version`.
///
/// Includes engine version, API version, Go version, OS/arch, and build
/// metadata.  Useful for verifying the Docker Engine version on a remote node
/// without shell access.
async fn version(
	State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
	let v = state
		.docker
		.version()
		.await
		.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
	Ok(Json(
		serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
	))
}

/// `GET /inspect/:container` — returns the full `docker inspect` output for a
/// single container.
///
/// Useful for checking health status, mounts, environment variables, and
/// labels without needing shell access to the node.
/// Returns `404` when the container does not exist or the daemon returns an
/// error.
async fn inspect(
	State(state): State<AppState>,
	Path(container): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
	let opts = Some(InspectContainerOptions { size: false });
	let detail = state
		.docker
		.inspect_container(&container, opts)
		.await
		.map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
	Ok(Json(
		serde_json::to_value(detail).unwrap_or(serde_json::Value::Null),
	))
}

// ── Logs ──────────────────────────────────────────────────────────────────────

/// Query parameters accepted by `GET /logs/:container`.
#[derive(Debug, Deserialize, Default)]
pub struct LogsQuery {
	/// Return only log lines produced after this point in time.
	///
	/// Accepts a Unix timestamp (integer seconds since epoch) or an
	/// RFC 3339 datetime string.  Omit to return all available logs.
	pub since: Option<String>,
}

/// `GET /logs/:container` — streams stdout + stderr for a container.
///
/// Timestamps are included in every line.  The response is capped at
/// [`Config::logs_max_bytes`]; exceeding the limit returns `413`.
async fn logs(
	State(state): State<AppState>,
	Path(container): Path<String>,
	Query(q): Query<LogsQuery>,
) -> Result<Json<String>, (StatusCode, String)> {
	let since = parse_since_unix(&q.since).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

	let opts = Some(LogsOptions::<String> {
		follow: false,
		stdout: true,
		stderr: true,
		since,
		until: 0,
		timestamps: true,
		tail: "all".into(),
	});

	let mut stream = state.docker.logs(&container, opts);
	let limit = state.config.logs_max_bytes;
	let mut buf = BytesMut::with_capacity(limit.min(8192));

	while let Some(chunk) = stream.next().await {
		let chunk = chunk.map_err(|e| {
			(
				StatusCode::BAD_REQUEST,
				format!("Cannot obtain container logs: {e}"),
			)
		})?;
		let b = chunk.into_bytes();
		if buf.len() + b.len() > limit {
			return Err((
				StatusCode::PAYLOAD_TOO_LARGE,
				format!("Log response exceeds LOGS_MAX_BYTES ({limit})"),
			));
		}
		buf.extend_from_slice(&b);
	}

	let s = String::from_utf8_lossy(&buf).into_owned();
	Ok(Json(s))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parses the `since` query parameter into a Unix timestamp.
///
/// Accepts:
/// - Missing / empty → `0` (no lower bound).
/// - Integer string → parsed directly as Unix seconds.
/// - RFC 3339 string → converted to Unix seconds.
fn parse_since_unix(raw: &Option<String>) -> Result<i64, String> {
	let Some(s) = raw.as_ref().map(|x| x.trim()).filter(|x| !x.is_empty()) else {
		return Ok(0);
	};
	if let Ok(v) = s.parse::<i64>() {
		return Ok(v);
	}
	chrono::DateTime::parse_from_rfc3339(s)
		.map(|d| d.timestamp())
		.map_err(|_| "invalid since (use unix seconds or RFC3339)".to_string())
}

fn header_str(h: Option<&axum::http::HeaderValue>) -> String {
	h.and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

/// Middleware that logs one structured line per request at `DEBUG` level.
async fn access_log(
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	req: Request<Body>,
	next: Next,
) -> Response {
	let start = Instant::now();
	let method = req.method().clone();
	let uri = req.uri().clone();
	let referer = header_str(req.headers().get(REFERER));
	let ua = header_str(req.headers().get(USER_AGENT));
	let resp = next.run(req).await;
	let status = resp.status().as_u16();
	let written = resp
		.headers()
		.get(axum::http::header::CONTENT_LENGTH)
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.parse::<u64>().ok())
		.unwrap_or(0);
	debug!(
		client = %addr.ip(),
		request = %format!("{method} {uri} HTTP/1.1"),
		status,
		written,
		referer = %referer,
		user_agent = %ua,
		elapsed_ms = %start.elapsed().as_millis(),
		"request"
	);
	resp
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_since_missing_or_empty_is_zero() {
		assert_eq!(parse_since_unix(&None).unwrap(), 0);
		assert_eq!(parse_since_unix(&Some("".into())).unwrap(), 0);
		assert_eq!(parse_since_unix(&Some("  ".into())).unwrap(), 0);
	}

	#[test]
	fn parse_since_unix_seconds() {
		assert_eq!(
			parse_since_unix(&Some("1700000000".into())).unwrap(),
			1_700_000_000
		);
	}

	#[test]
	fn parse_since_rfc3339() {
		let ts = parse_since_unix(&Some("2024-01-15T12:00:00Z".into())).unwrap();
		assert_eq!(ts, 1_705_320_000);
	}

	#[test]
	fn parse_since_rejects_invalid() {
		assert!(parse_since_unix(&Some("not-a-date".into())).is_err());
	}
}
