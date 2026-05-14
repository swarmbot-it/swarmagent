//! HTTP sink to Swarmbot (`/events`) with a shared `reqwest::Client`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use reqwest::header::CONTENT_TYPE;
use serde::Serialize;
use tracing::{error, info};

use crate::config::Config;

const JSON_UTF8: &str = "application/json; charset=utf-8";

#[derive(Clone)]
pub struct Sink {
    cfg: Arc<Config>,
    client: reqwest::Client,
}

impl Sink {
    pub fn new(cfg: Arc<Config>, client: reqwest::Client) -> Self {
        Self { cfg, client }
    }

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
                    info!("Swarmbot OK");
                    break;
                }
                Ok(r) => {
                    error!(status = %r.status(), "Swarmbot health check returned non-success");
                }
                Err(e) => {
                    error!(error = %e, "Swarmbot health check failed");
                }
            }
        }
    }

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
            .context("POST event to Swarmbot")?;
        Ok(())
    }
}

pub fn build_http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        .user_agent(concat!("swarmagent/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build reqwest client")
}
