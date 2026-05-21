//! Periodic host and container statistics collector.
//!
//! On every tick this task:
//! 1. Fetches `docker info` and `docker version` concurrently.
//! 2. Samples host CPU, memory, and disk usage via `sysinfo`.
//! 3. Lists all running containers and fetches per-container stats in
//!    parallel (bounded by [`Config::stats_max_concurrency`]).
//! 4. Assembles a [`Status`] payload and forwards it to Swarmboty.

use std::sync::Arc;

use anyhow::Context;
use bollard::container::{ListContainersOptions, StatsOptions};
use bollard::Docker;
use futures_util::StreamExt;
use tokio::sync::Semaphore;
use tracing::error;

use crate::config::Config;
use crate::container_stats::container_status_from_stats;
use crate::host;
use crate::models::{ContainerStatus, Status};
use crate::sink::Sink;

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
		Some(Ok(stats)) => container_status_from_stats(&stats, os_type),
		Some(Err(e)) => {
			error!(container = %id, error = %e, "Statistics fetching failed");
			ContainerStatus::empty(id)
		}
		None => ContainerStatus::empty(id),
	}
}

/// Runs the stats collector indefinitely, ticking at the configured interval.
///
/// Missed ticks are delayed rather than bunched up, so a slow tick does not
/// immediately trigger another one.
pub async fn run(docker: Docker, sink: Arc<Sink>, cfg: Arc<Config>) {
	let mut ticker = tokio::time::interval(cfg.stats_frequency);
	ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
	ticker.tick().await;

	loop {
		ticker.tick().await;
		if let Err(e) = tick(&docker, &sink, &cfg).await {
			error!(error = %e, "stats tick failed");
		}
	}
}

/// Performs a single stats tick: collects host and container metrics, then
/// posts them to Swarmboty.
async fn tick(docker: &Docker, sink: &Sink, cfg: &Config) -> anyhow::Result<()> {
	// Fetch daemon metadata concurrently to minimise added latency per tick.
	let (info, ver) =
		tokio::try_join!(docker.info(), docker.version(),).context("docker info / version")?;

	let node_id = info
		.swarm
		.as_ref()
		.and_then(|s| s.node_id.clone())
		.unwrap_or_default();
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

	let status = Status {
		id: node_id,
		disk,
		cpu,
		memory,
		tasks,
		engine_version,
		api_version,
		kernel_version,
	};
	sink.post_event("stats", &status).await?;
	Ok(())
}
