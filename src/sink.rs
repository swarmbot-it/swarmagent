//! HTTP sink — forwards JSON event payloads to Swarmboty's `/events` endpoint.
//!
//! [`Sink`] wraps a shared [`reqwest::Client`] and the agent configuration.
//! All outbound requests are fire-and-forget: errors are logged but do not
//! propagate to the caller's event loop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use reqwest::header::CONTENT_TYPE;
use serde::Serialize;
use tracing::{error, info};

use crate::config::Config;

const JSON_UTF8: &str = "application/json; charset=utf-8";

/// Shared HTTP client for forwarding events to Swarmboty.
#[derive(Clone)]
pub struct Sink {
	cfg: Arc<Config>,
	client: reqwest::Client,
}

impl Sink {
	/// Creates a new [`Sink`] backed by the provided config and HTTP client.
	pub fn new(cfg: Arc<Config>, client: reqwest::Client) -> Self {
		Self { cfg, client }
	}

	/// Blocks until Swarmboty's health-check endpoint returns HTTP 2xx.
	///
	/// Polls every 5 seconds. Intended to be called once at startup before
	/// spawning background tasks so that initial events are not lost.
	pub async fn wait_for_health(&self) {
		loop {
			tokio::time::sleep(Duration::from_secs(5)).await;
			match self
				.client
				.get(&self.cfg.health_check_endpoint)
				.send()
				.await
			{
				Ok(r) if r.status().is_success() => {
					info!("Swarmboty OK");
					break;
				}
				Ok(r) => {
					error!(status = %r.status(), "Swarmboty health check returned non-success");
				}
				Err(e) => {
					error!(error = %e, "Swarmboty health check failed");
				}
			}
		}
	}

	/// Serializes `message` into the standard event envelope and POSTs it to
	/// the configured `event_endpoint`.
	///
	/// The envelope format is:
	/// ```json
	/// { "type": "<ty>", "message": <message> }
	/// ```
	///
	/// When the relevant debug flag is set, the raw JSON is emitted at
	/// `TRACE`/`DEBUG` level before the request is sent.
	pub async fn post_event(&self, ty: &str, message: &impl Serialize) -> anyhow::Result<()> {
		let body = serde_json::json!({
			"type": ty,
			"message": serde_json::to_value(message)?,
		});
		let payload = serde_json::to_vec(&body).context("serialize outbound event")?;

		if self.cfg.debug_event && ty == "event" {
			tracing::debug!(target: "swarmagent", body = %String::from_utf8_lossy(&payload), "Docker event");
		}
		if self.cfg.debug_stats && ty == "stats" {
			tracing::debug!(target: "swarmagent", body = %String::from_utf8_lossy(&payload), "Host stats");
		}

		self.client
			.post(&self.cfg.event_endpoint)
			.header(CONTENT_TYPE, JSON_UTF8)
			.body(payload)
			.send()
			.await
			.context("POST event to Swarmboty")?;
		Ok(())
	}
}

/// Builds the shared [`reqwest::Client`] used by [`Sink`].
///
/// The client is configured with a 90-second idle-connection timeout and a
/// `User-Agent` header that identifies the agent version.
pub fn build_http_client() -> anyhow::Result<reqwest::Client> {
	reqwest::Client::builder()
		.pool_idle_timeout(Duration::from_secs(90))
		.pool_max_idle_per_host(4)
		.user_agent(concat!("swarmagent/", env!("CARGO_PKG_VERSION")))
		.build()
		.context("build reqwest client")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn build_http_client_ok() {
		build_http_client().expect("reqwest client");
	}
}
