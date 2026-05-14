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
use bollard::container::LogsOptions;
use bollard::Docker;
use bytes::BytesMut;
use futures_util::StreamExt;
use serde::Deserialize;
use tracing::debug;

use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub docker: Docker,
    pub config: Arc<Config>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(info))
        .route("/logs/:container", get(logs))
        .layer(middleware::from_fn(access_log))
        .with_state(state)
}

async fn info(State(state): State<AppState>) -> Json<crate::config::InfoResponse> {
    Json(state.config.to_info_json())
}

#[derive(Debug, Deserialize, Default)]
pub struct LogsQuery {
    pub since: Option<String>,
}

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
