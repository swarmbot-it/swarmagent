//! Periodic statistics collector.
//!
//! On every tick the configured [`Provider`] assembles a full
//! [`crate::models::Status`] snapshot (node identity, host CPU/memory/disk,
//! per-container statistics) which is forwarded to Swarmboty as a `stats`
//! event.

use std::sync::Arc;

use tracing::error;

use crate::config::Config;
use crate::provider::Provider;
use crate::sink::Sink;

/// Runs the stats collector indefinitely, ticking at the configured interval.
///
/// Posts one sample immediately on startup so the API and UI have data without
/// waiting a full [`Config::stats_frequency`] interval. Missed ticks are delayed
/// rather than bunched up, so a slow tick does not immediately trigger another.
pub async fn run(provider: Arc<dyn Provider>, sink: Arc<Sink>, cfg: Arc<Config>) {
	if let Err(e) = tick(provider.as_ref(), &sink).await {
		error!(error = %e, "stats tick failed");
	}

	let mut ticker = tokio::time::interval(cfg.stats_frequency);
	ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
	ticker.tick().await;

	loop {
		ticker.tick().await;
		if let Err(e) = tick(provider.as_ref(), &sink).await {
			error!(error = %e, "stats tick failed");
		}
	}
}

/// Performs a single stats tick: collects the snapshot from the provider and
/// posts it to Swarmboty.
async fn tick(provider: &dyn Provider, sink: &Sink) -> anyhow::Result<()> {
	let status = provider.status().await?;
	sink.post_event("stats", &status).await
}
