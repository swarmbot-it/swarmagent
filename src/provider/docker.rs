//! Docker Engine provider — Swarm nodes and standalone daemons.
//!
//! Statistics come from `docker info`/`docker version`/`docker stats`
//! (via bollard) plus host sampling through `sysinfo`. Events come from
//! `docker events` with automatic reconnection and exponential back-off
//! (1 s → 2 s → … → 60 s cap) so the agent survives a Docker daemon restart
//! without manual intervention.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bollard::container::{ListContainersOptions, StatsOptions};
use bollard::system::EventsOptions;
use bollard::Docker;
use futures_util::StreamExt;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::container_stats::container_status_from_stats;
use crate::host;
use crate::models::{ContainerStatus, Status};
use crate::provider::Provider;
use crate::sink::Sink;

/// Docker event types forwarded to Swarmbot.
///
/// Filtering here reduces noise from build/plugin events in active CI hosts.
const RELEVANT_TYPES: &[&str] = &[
	"container",
	"network",
	"service",
	"node",
	"secret",
	"config",
	"volume",
];

/// [`Provider`] implementation backed by a local Docker Engine.
pub struct DockerProvider {
	docker: Docker,
	cfg: Arc<Config>,
}

impl DockerProvider {
	/// Connects to the Docker daemon (default socket or `DOCKER_HOST`) and
	/// negotiates the API version.
	pub async fn connect(cfg: Arc<Config>) -> anyhow::Result<Self> {
		let docker = Docker::connect_with_local_defaults()
			.context("Docker client (check DOCKER_HOST / socket)")?
			.negotiate_version()
			.await
			.context("Docker API version negotiation")?;

		match docker.version().await {
			Ok(v) => info!(
				engine = %v.version.as_deref().unwrap_or("?"),
				api = %v.api_version.as_deref().unwrap_or("?"),
				"Docker Engine connected"
			),
			Err(e) => warn!(error = %e, "Could not fetch Docker version"),
		}

		Ok(Self { docker, cfg })
	}
}

#[async_trait::async_trait]
impl Provider for DockerProvider {
	fn orchestrator(&self) -> &'static str {
		"swarm"
	}

	async fn status(&self) -> anyhow::Result<Status> {
		collect_status(&self.docker, &self.cfg).await
	}

	async fn run_events(&self, sink: Arc<Sink>) {
		let mut delay = Duration::from_secs(1);
		loop {
			info!("Docker event stream starting");
			let clean = drain_stream(&self.docker, &sink).await;
			if clean {
				warn!("Docker event stream closed; reconnecting in {:?}", delay);
			} else {
				error!("Docker event stream failed; reconnecting in {:?}", delay);
			}
			tokio::time::sleep(delay).await;
			delay = (delay * 2).min(Duration::from_secs(60));
		}
	}
}

/// Builds the [`EventsOptions`] filter that restricts the stream to
/// [`RELEVANT_TYPES`].
fn events_options() -> EventsOptions<String> {
	let mut filters: HashMap<String, Vec<String>> = HashMap::new();
	filters.insert(
		"type".into(),
		RELEVANT_TYPES.iter().map(|s| s.to_string()).collect(),
	);
	EventsOptions {
		since: None,
		until: None,
		filters,
	}
}

/// Drains one Docker event stream until it errors or the daemon closes it.
///
/// Returns `true` when the stream ended cleanly (EOF without an error),
/// `false` when an error was received from the stream.
async fn drain_stream(docker: &Docker, sink: &Arc<Sink>) -> bool {
	let mut stream = docker.events(Some(events_options()));
	while let Some(item) = stream.next().await {
		match item {
			Ok(msg) => {
				if let Err(e) = sink.post_event("event", &msg).await {
					error!(error = %e, "Event forwarding failed");
				}
			}
			Err(e) => {
				error!(error = %e, "Docker event stream error");
				return false;
			}
		}
	}
	// Stream closed without an error (e.g. daemon restart).
	false
}

/// Fetches a single one-shot stats snapshot for `id`.
///
/// Returns an empty [`ContainerStatus`] when the container has stopped
/// between list and stats, or when the Docker API returns an error.
async fn container_stats_one(docker: &Docker, id: &str, os_type: &str) -> ContainerStatus {
	let opts = Some(StatsOptions {
		stream: false,
		one_shot: true,
	});
	let mut stream = docker.stats(id, opts);
	match stream.next().await {
		Some(Ok(stats)) => container_status_from_stats(&stats, os_type, id),
		Some(Err(e)) => {
			error!(container = %id, error = %e, "Statistics fetching failed");
			ContainerStatus::empty(id)
		}
		None => ContainerStatus::empty(id),
	}
}

/// Collects one full [`Status`] snapshot: daemon metadata, host metrics via
/// `sysinfo`, and per-container statistics fetched in parallel (bounded by
/// [`Config::stats_max_concurrency`]).
async fn collect_status(docker: &Docker, cfg: &Config) -> anyhow::Result<Status> {
	// Fetch daemon metadata concurrently to minimise added latency per tick.
	let (info, ver) =
		tokio::try_join!(docker.info(), docker.version(),).context("docker info / version")?;

	let node_id = info
		.swarm
		.as_ref()
		.and_then(|s| s.node_id.clone())
		.unwrap_or_default();
	let hostname = info.name.clone().unwrap_or_default();
	let ncpu = info.ncpu.unwrap_or(1) as i32;
	let os_type = info.os_type.clone().unwrap_or_else(|| "linux".to_string());

	let engine_version = ver
		.version
		.clone()
		.unwrap_or_else(|| info.server_version.clone().unwrap_or_default());
	let api_version = ver.api_version.clone().unwrap_or_default();
	let kernel_version = ver
		.kernel_version
		.clone()
		.unwrap_or_else(|| info.kernel_version.clone().unwrap_or_default());

	let mut sys = host::new_system();
	let memory = host::memory_usage(&mut sys);
	let disk = host::disk_usage();
	let cpu = host::cpu_usage(&mut sys, ncpu);

	let list_opts = Some(ListContainersOptions::<String>::default());
	let summaries = docker.list_containers(list_opts).await.unwrap_or_default();
	let ids: Vec<String> = summaries.into_iter().filter_map(|c| c.id).collect();

	let sem = Arc::new(Semaphore::new(cfg.stats_max_concurrency));
	let mut join = tokio::task::JoinSet::new();
	for id in ids {
		let d = docker.clone();
		let sem = sem.clone();
		let os = os_type.clone();
		join.spawn(async move {
			let _permit = sem
				.acquire_owned()
				.await
				.expect("stats semaphore should stay open");
			container_stats_one(&d, &id, &os).await
		});
	}

	let mut tasks = Vec::new();
	while let Some(res) = join.join_next().await {
		match res {
			Ok(s) => tasks.push(s),
			Err(e) => error!(error = %e, "stats task join"),
		}
	}

	Ok(Status {
		id: node_id,
		hostname,
		orchestrator: "swarm".to_string(),
		disk,
		cpu,
		memory,
		tasks,
		engine_version,
		api_version,
		kernel_version,
		agent_version: env!("CARGO_PKG_VERSION").to_string(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn events_options_has_type_filter() {
		let opts = events_options();
		let types = opts
			.filters
			.get("type")
			.expect("type filter must be present");
		for t in RELEVANT_TYPES {
			assert!(types.contains(&t.to_string()), "missing type: {t}");
		}
		assert_eq!(types.len(), RELEVANT_TYPES.len());
	}

	#[test]
	fn events_options_excludes_build_and_plugin() {
		let opts = events_options();
		let types = opts.filters.get("type").unwrap();
		assert!(!types.contains(&"build".to_string()));
		assert!(!types.contains(&"plugin".to_string()));
		assert!(!types.contains(&"image".to_string()));
	}

	#[test]
	fn relevant_types_count() {
		assert_eq!(RELEVANT_TYPES.len(), 7);
	}
}
