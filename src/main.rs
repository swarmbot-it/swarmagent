mod config;
mod container_stats;
mod detect;
mod host;
mod models;
mod provider;
mod sink;
mod tasks;

use std::sync::Arc;

use tracing::info;

use crate::config::Config;
use crate::provider::Provider;
use crate::sink::{build_http_client, Sink};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	tracing_subscriber::fmt()
		.with_env_filter(
			tracing_subscriber::EnvFilter::try_from_default_env()
				.unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
		)
		.init();

	let config = Arc::new(Config::from_env());
	let http = build_http_client()?;

	let mode = detect::resolve(config.agent_mode, detect::in_cluster());
	info!(mode = mode.as_str(), "Orchestrator resolved");

	let provider: Arc<dyn Provider> = match mode {
		detect::Mode::Docker => {
			Arc::new(provider::docker::DockerProvider::connect(config.clone()).await?)
		}
		detect::Mode::Kubernetes => Arc::new(provider::kubernetes::KubernetesProvider::from_env(
			config.clone(),
		)?),
	};

	info!(orchestrator = provider.orchestrator(), "Provider ready");

	info!("Waiting for Swarmboty…");
	let sink = Sink::new(config.clone(), http);
	sink.wait_for_health().await;

	let sink = Arc::new(sink);

	{
		let provider = provider.clone();
		let sink = sink.clone();
		tokio::spawn(async move { provider.run_events(sink).await });
	}
	info!("Event collector started.");

	tokio::spawn(tasks::stats::run(
		provider.clone(),
		sink.clone(),
		config.clone(),
	));
	info!("Stats collector started.");

	// Push-only agent: no listening sockets. Park the main task until the
	// process is asked to stop.
	tokio::signal::ctrl_c().await?;
	info!("Shutdown signal received; exiting.");
	Ok(())
}
