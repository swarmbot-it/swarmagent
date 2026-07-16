//! kubelet Summary API (`/stats/summary`) — types and mapping to agent models.
//!
//! The Summary API is the single source for both node-level metrics
//! (CPU/memory/rootfs — no `/proc` mounts or `hostPID` needed) and
//! per-container metrics of every pod scheduled on the node.
//!
//! Known gaps versus Docker stats, accepted by design:
//! - network counters exist per **pod**, not per container — they are
//!   attributed to the pod's first container so cluster totals stay correct;
//! - block I/O and PID counts are not exposed — reported as `0` (Swarmboty
//!   does not consume them today).

use std::collections::HashMap;

use serde::Deserialize;

use crate::models::{ContainerStatus, CpuStatus, DiskStatus, MemoryStatus};
use crate::provider::kubernetes::pods::PodMeta;

/// Root object returned by `GET /stats/summary`.
#[derive(Debug, Deserialize)]
pub struct Summary {
	/// Node-level aggregates.
	pub node: NodeSummary,
	/// Per-pod statistics for pods on this node.
	#[serde(default)]
	pub pods: Vec<PodSummary>,
}

/// Node section of the summary.
#[derive(Debug, Default, Deserialize)]
pub struct NodeSummary {
	/// CPU usage of the whole node.
	pub cpu: Option<CpuSample>,
	/// Memory usage of the whole node.
	pub memory: Option<MemorySample>,
	/// Root filesystem usage of the node.
	pub fs: Option<FsSample>,
}

/// One pod's statistics.
#[derive(Debug, Deserialize)]
pub struct PodSummary {
	/// Pod identity (name, namespace).
	#[serde(rename = "podRef")]
	pub pod_ref: PodRef,
	/// Per-container samples.
	#[serde(default)]
	pub containers: Vec<ContainerSample>,
	/// Pod-level network counters (containers share the pod netns).
	pub network: Option<NetworkSample>,
}

/// Pod reference inside the summary.
#[derive(Debug, Default, Deserialize)]
pub struct PodRef {
	/// Pod name.
	#[serde(default)]
	pub name: String,
	/// Pod namespace.
	#[serde(default)]
	pub namespace: String,
}

/// One container's samples.
#[derive(Debug, Deserialize)]
pub struct ContainerSample {
	/// Container name (unique within the pod).
	#[serde(default)]
	pub name: String,
	/// CPU sample.
	pub cpu: Option<CpuSample>,
	/// Memory sample.
	pub memory: Option<MemorySample>,
}

/// CPU usage sample.
#[derive(Debug, Default, Deserialize)]
pub struct CpuSample {
	/// Cumulative-free instantaneous usage in nano-cores.
	#[serde(rename = "usageNanoCores")]
	pub usage_nano_cores: Option<u64>,
}

/// Memory usage sample.
#[derive(Debug, Default, Deserialize)]
pub struct MemorySample {
	/// Bytes still available before hitting memory pressure (node level).
	#[serde(rename = "availableBytes")]
	pub available_bytes: Option<u64>,
	/// Working set — the value the kubelet uses for eviction decisions.
	#[serde(rename = "workingSetBytes")]
	pub working_set_bytes: Option<u64>,
}

/// Filesystem usage sample.
#[derive(Debug, Default, Deserialize)]
pub struct FsSample {
	/// Total capacity in bytes.
	#[serde(rename = "capacityBytes")]
	pub capacity_bytes: Option<u64>,
	/// Used bytes.
	#[serde(rename = "usedBytes")]
	pub used_bytes: Option<u64>,
	/// Available bytes.
	#[serde(rename = "availableBytes")]
	pub available_bytes: Option<u64>,
}

/// Pod network counters.
#[derive(Debug, Default, Deserialize)]
pub struct NetworkSample {
	/// Cumulative received bytes on the default interface.
	#[serde(rename = "rxBytes")]
	pub rx_bytes: Option<u64>,
	/// Cumulative transmitted bytes on the default interface.
	#[serde(rename = "txBytes")]
	pub tx_bytes: Option<u64>,
	/// Per-interface counters (fallback when the top-level fields are absent).
	#[serde(default)]
	pub interfaces: Vec<InterfaceSample>,
}

/// One network interface's counters.
#[derive(Debug, Default, Deserialize)]
pub struct InterfaceSample {
	/// Cumulative received bytes.
	#[serde(rename = "rxBytes")]
	pub rx_bytes: Option<u64>,
	/// Cumulative transmitted bytes.
	#[serde(rename = "txBytes")]
	pub tx_bytes: Option<u64>,
}

impl NetworkSample {
	/// `(rx, tx)` — top-level counters, or the sum over interfaces.
	fn totals(&self) -> (u64, u64) {
		match (self.rx_bytes, self.tx_bytes) {
			(Some(rx), Some(tx)) => (rx, tx),
			_ => self.interfaces.iter().fold((0, 0), |(rx, tx), i| {
				(
					rx.saturating_add(i.rx_bytes.unwrap_or(0)),
					tx.saturating_add(i.tx_bytes.unwrap_or(0)),
				)
			}),
		}
	}
}

/// Percentage helper — `0.0` when the denominator is zero.
fn pct(used: f64, total: f64) -> f64 {
	if total > 0.0 {
		used / total * 100.0
	} else {
		0.0
	}
}

/// Maps the node section of the summary to host-level status models.
///
/// `capacity_cores` and `capacity_mem_bytes` come from the Node object
/// (`status.capacity`), which the provider fetches in the same tick.
pub fn node_status_parts(
	node: &NodeSummary,
	capacity_cores: f64,
	capacity_mem_bytes: u64,
) -> (CpuStatus, MemoryStatus, DiskStatus) {
	let usage_nano = node
		.cpu
		.as_ref()
		.and_then(|c| c.usage_nano_cores)
		.unwrap_or(0);
	let cpu = CpuStatus {
		used_percentage: pct(usage_nano as f64, capacity_cores * 1e9),
		cores: (capacity_cores.round() as i32).max(1),
	};

	let total = capacity_mem_bytes;
	let (available, working_set) = node
		.memory
		.as_ref()
		.map(|m| (m.available_bytes, m.working_set_bytes))
		.unwrap_or((None, None));
	let free = available.unwrap_or_else(|| total.saturating_sub(working_set.unwrap_or(0)));
	let used = total.saturating_sub(free);
	let memory = MemoryStatus {
		total,
		used,
		used_percentage: pct(used as f64, total as f64),
		free,
	};

	let (fs_total, fs_used, fs_free) = node
		.fs
		.as_ref()
		.map(|f| {
			let total = f.capacity_bytes.unwrap_or(0);
			let free = f.available_bytes.unwrap_or(0);
			let used = f.used_bytes.unwrap_or_else(|| total.saturating_sub(free));
			(total, used, free)
		})
		.unwrap_or((0, 0, 0));
	let disk = DiskStatus {
		total: fs_total,
		used: fs_used,
		used_percentage: pct(fs_used as f64, fs_total as f64),
		free: fs_free,
	};

	(cpu, memory, disk)
}

/// Maps per-pod container samples to [`ContainerStatus`] entries.
///
/// `metas` is keyed by `"{namespace}/{pod}"` (see
/// [`crate::provider::kubernetes::pods::parse_pod_metas`]); container IDs are
/// `"{namespace}/{pod}/{container}"`. CPU percentages are relative to the
/// whole node (consistent with the Docker provider).
pub fn container_statuses(
	summary: &Summary,
	metas: &HashMap<String, PodMeta>,
	capacity_cores: f64,
) -> Vec<ContainerStatus> {
	let mut out = Vec::new();
	for pod in &summary.pods {
		let namespace = pod.pod_ref.namespace.as_str();
		let pod_name = pod.pod_ref.name.as_str();
		if namespace.is_empty() || pod_name.is_empty() {
			continue;
		}
		let meta = metas.get(&format!("{namespace}/{pod_name}"));
		let (pod_rx, pod_tx) = pod
			.network
			.as_ref()
			.map(NetworkSample::totals)
			.unwrap_or((0, 0));

		for (idx, c) in pod.containers.iter().enumerate() {
			if c.name.is_empty() {
				continue;
			}
			let usage_nano = c.cpu.as_ref().and_then(|s| s.usage_nano_cores).unwrap_or(0);
			let working_set = c
				.memory
				.as_ref()
				.and_then(|m| m.working_set_bytes)
				.unwrap_or(0) as f64;
			let limit = meta
				.and_then(|m| m.mem_limits.get(&c.name))
				.copied()
				.unwrap_or(0) as f64;

			// Pod-level network counters are attributed to the first
			// container so that sums over all containers stay correct.
			let (rx, tx) = if idx == 0 { (pod_rx, pod_tx) } else { (0, 0) };

			out.push(ContainerStatus {
				name: c.name.clone(),
				id: format!("{namespace}/{pod_name}/{}", c.name),
				namespace: Some(namespace.to_string()),
				pod: Some(pod_name.to_string()),
				workload: meta.and_then(|m| m.workload.clone()),
				workload_kind: meta.and_then(|m| m.workload_kind.clone()),
				cpu_percentage: pct(usage_nano as f64, capacity_cores * 1e9),
				memory: working_set,
				memory_limit: limit,
				memory_percentage: pct(working_set, limit),
				network_rx_bytes: rx,
				network_tx_bytes: tx,
				block_read_bytes: 0,
				block_write_bytes: 0,
				pids: 0,
			});
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	fn sample_summary() -> Summary {
		serde_json::from_value(json!({
			"node": {
				"nodeName": "k3s-1",
				"cpu": {"usageNanoCores": 500_000_000_u64},
				"memory": {"availableBytes": 6_000_000_000_u64, "workingSetBytes": 2_000_000_000_u64},
				"fs": {"capacityBytes": 100_000_000_000_u64, "usedBytes": 40_000_000_000_u64, "availableBytes": 60_000_000_000_u64}
			},
			"pods": [{
				"podRef": {"name": "web-7f9c6b7d54-abcde", "namespace": "prod"},
				"containers": [
					{"name": "web", "cpu": {"usageNanoCores": 250_000_000_u64}, "memory": {"workingSetBytes": 134_217_728_u64}},
					{"name": "sidecar", "cpu": {}, "memory": {"workingSetBytes": 1_048_576_u64}}
				],
				"network": {"rxBytes": 1000, "txBytes": 2000}
			}]
		}))
		.expect("summary parses")
	}

	#[test]
	fn node_parts_compute_percentages() {
		let s = sample_summary();
		let (cpu, mem, disk) = node_status_parts(&s.node, 4.0, 8_000_000_000);
		// 0.5 core of 4 → 12.5 %
		assert!((cpu.used_percentage - 12.5).abs() < 1e-9);
		assert_eq!(cpu.cores, 4);
		assert_eq!(mem.total, 8_000_000_000);
		assert_eq!(mem.free, 6_000_000_000);
		assert_eq!(mem.used, 2_000_000_000);
		assert!((mem.used_percentage - 25.0).abs() < 1e-9);
		assert_eq!(disk.total, 100_000_000_000);
		assert_eq!(disk.used, 40_000_000_000);
		assert!((disk.used_percentage - 40.0).abs() < 1e-9);
	}

	#[test]
	fn node_parts_tolerate_empty_summary() {
		let (cpu, mem, disk) = node_status_parts(&NodeSummary::default(), 0.0, 0);
		assert_eq!(cpu.used_percentage, 0.0);
		assert_eq!(cpu.cores, 1);
		assert_eq!(mem.used_percentage, 0.0);
		assert_eq!(disk.total, 0);
	}

	#[test]
	fn containers_map_ids_metrics_and_metadata() {
		let s = sample_summary();
		let mut metas = HashMap::new();
		metas.insert(
			"prod/web-7f9c6b7d54-abcde".to_string(),
			PodMeta {
				workload: Some("web".into()),
				workload_kind: Some("Deployment".into()),
				mem_limits: HashMap::from([("web".to_string(), 268_435_456_u64)]),
			},
		);
		let list = container_statuses(&s, &metas, 4.0);
		assert_eq!(list.len(), 2);

		let web = &list[0];
		assert_eq!(web.id, "prod/web-7f9c6b7d54-abcde/web");
		assert_eq!(web.name, "web");
		assert_eq!(web.namespace.as_deref(), Some("prod"));
		assert_eq!(web.workload.as_deref(), Some("web"));
		assert_eq!(web.workload_kind.as_deref(), Some("Deployment"));
		// 0.25 core of 4 → 6.25 %
		assert!((web.cpu_percentage - 6.25).abs() < 1e-9);
		// 128 MiB of a 256 MiB limit → 50 %
		assert!((web.memory_percentage - 50.0).abs() < 1e-9);
		// pod network attributed to the first container only
		assert_eq!(web.network_rx_bytes, 1000);
		assert_eq!(list[1].network_rx_bytes, 0);

		let sidecar = &list[1];
		assert_eq!(sidecar.memory_limit, 0.0);
		assert_eq!(sidecar.memory_percentage, 0.0);
	}

	#[test]
	fn network_totals_fall_back_to_interfaces() {
		let net: NetworkSample = serde_json::from_value(json!({
			"interfaces": [
				{"rxBytes": 10, "txBytes": 20},
				{"rxBytes": 1, "txBytes": 2}
			]
		}))
		.unwrap();
		assert_eq!(net.totals(), (11, 22));
	}
}
