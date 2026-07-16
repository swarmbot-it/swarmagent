//! Kubernetes (k3s) provider.
//!
//! Runs as a DaemonSet pod. Per stats tick it reads:
//!
//! 1. its own Node object (identity, versions, capacity),
//! 2. the kubelet Summary API (node + per-container metrics) — directly on
//!    port 10250 or through the API server proxy (see
//!    [`crate::config::KubeletMode`]),
//! 3. the list of pods scheduled on the node (memory limits, workload owners).
//!
//! Events come from a pod watch scoped to this node (see [`events`]).

mod client;
mod events;
mod pods;
mod summary;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use serde_json::Value;
use tracing::{error, info, warn};

use crate::config::{Config, KubeletMode};
use crate::models::Status;
use crate::provider::Provider;
use crate::sink::Sink;

use client::KubeClient;
use events::{diff_watch_event, WatchState};
use pods::{parse_pod_metas, parse_quantity_bytes, parse_quantity_cores};
use summary::{container_statuses, node_status_parts, Summary};

/// [`Provider`] implementation backed by the Kubernetes API.
pub struct KubernetesProvider {
	client: KubeClient,
	cfg: Arc<Config>,
	node_name: String,
}

impl KubernetesProvider {
	/// Builds the provider from the in-cluster environment.
	///
	/// Requires `NODE_NAME` (Downward API, `fieldRef: spec.nodeName`) so the
	/// agent knows which node it represents.
	pub fn from_env(cfg: Arc<Config>) -> anyhow::Result<Self> {
		let node_name = cfg.node_name.clone().context(
			"NODE_NAME is required in Kubernetes mode \
			 (set it via the Downward API: fieldRef: spec.nodeName)",
		)?;
		let client = KubeClient::in_cluster(cfg.kubelet_insecure_tls)?;
		info!(node = %node_name, "Kubernetes provider initialised");
		Ok(Self {
			client,
			cfg,
			node_name,
		})
	}

	/// Fetches `stats/summary`, preferring the direct kubelet endpoint and
	/// falling back to the API server proxy.
	async fn fetch_summary(&self, node_ip: Option<&str>) -> anyhow::Result<Value> {
		if self.cfg.kubelet_mode == KubeletMode::Direct {
			let ip = self.cfg.node_ip.as_deref().or(node_ip);
			if let Some(ip) = ip {
				let url = format!("https://{ip}:10250/stats/summary");
				match self.client.kubelet_get_json(&url).await {
					Ok(v) => return Ok(v),
					Err(e) => warn!(
						error = %e,
						"direct kubelet summary failed; falling back to API server proxy"
					),
				}
			} else {
				warn!("no node IP known; using API server proxy for stats/summary");
			}
		}
		self.client
			.get_json(&format!(
				"/api/v1/nodes/{}/proxy/stats/summary",
				self.node_name
			))
			.await
	}

	/// One pass over the pod watch stream; returns when the stream closes.
	async fn watch_once(&self, state: &mut WatchState, sink: &Sink) -> anyhow::Result<()> {
		let path = format!(
			"/api/v1/pods?watch=1&fieldSelector=spec.nodeName={}",
			self.node_name
		);
		let resp = self.client.watch(&path).await?;
		let mut stream = resp.bytes_stream();
		let mut buf: Vec<u8> = Vec::new();

		while let Some(chunk) = stream.next().await {
			let chunk = chunk.context("pod watch stream")?;
			buf.extend_from_slice(&chunk);
			while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
				let line: Vec<u8> = buf.drain(..=pos).collect();
				let line = &line[..line.len().saturating_sub(1)];
				if line.iter().all(|b| b.is_ascii_whitespace()) {
					continue;
				}
				let parsed: Value = match serde_json::from_slice(line) {
					Ok(v) => v,
					Err(e) => {
						warn!(error = %e, "unparseable watch line");
						continue;
					}
				};
				let event_type = parsed.get("type").and_then(Value::as_str).unwrap_or("");
				let Some(object) = parsed.get("object") else {
					continue;
				};
				let now = chrono::Utc::now().timestamp();
				for ev in diff_watch_event(state, event_type, object, now) {
					if let Err(e) = sink.post_event("event", &ev).await {
						error!(error = %e, "Event forwarding failed");
					}
				}
			}
		}
		Ok(())
	}
}

#[async_trait::async_trait]
impl Provider for KubernetesProvider {
	fn orchestrator(&self) -> &'static str {
		"kubernetes"
	}

	async fn status(&self) -> anyhow::Result<Status> {
		let node = self
			.client
			.get_json(&format!("/api/v1/nodes/{}", self.node_name))
			.await?;
		let meta = parse_node_meta(&node);

		let summary_raw = self.fetch_summary(meta.internal_ip.as_deref()).await?;
		let summary: Summary =
			serde_json::from_value(summary_raw).context("parse stats/summary")?;

		let pod_list = self
			.client
			.get_json(&format!(
				"/api/v1/pods?fieldSelector=spec.nodeName={}",
				self.node_name
			))
			.await
			.unwrap_or_else(|e| {
				warn!(error = %e, "pod list failed; container metadata will be incomplete");
				Value::Null
			});
		let metas = parse_pod_metas(&pod_list);

		let (cpu, memory, disk) =
			node_status_parts(&summary.node, meta.capacity_cores, meta.capacity_mem_bytes);
		let tasks = container_statuses(&summary, &metas, meta.capacity_cores);

		Ok(Status {
			id: self.node_name.clone(),
			hostname: meta.hostname,
			orchestrator: "kubernetes".to_string(),
			disk,
			cpu,
			memory,
			tasks,
			engine_version: meta.engine_version,
			api_version: meta.api_version,
			kernel_version: meta.kernel_version,
			agent_version: env!("CARGO_PKG_VERSION").to_string(),
		})
	}

	async fn run_events(&self, sink: Arc<Sink>) {
		let mut delay = Duration::from_secs(1);
		let mut state = WatchState::new();
		loop {
			info!("Kubernetes pod watch starting");
			match self.watch_once(&mut state, &sink).await {
				Ok(()) => warn!("pod watch closed; reconnecting in {:?}", delay),
				Err(e) => error!(error = %e, "pod watch failed; reconnecting in {:?}", delay),
			}
			tokio::time::sleep(delay).await;
			delay = (delay * 2).min(Duration::from_secs(60));
		}
	}
}

/// Identity and capacity extracted from a `v1.Node` object.
#[derive(Debug, Default, PartialEq)]
struct NodeMeta {
	hostname: String,
	engine_version: String,
	api_version: String,
	kernel_version: String,
	capacity_cores: f64,
	capacity_mem_bytes: u64,
	internal_ip: Option<String>,
}

/// Extracts [`NodeMeta`] from a Node JSON object; missing fields degrade to
/// empty strings / zeros rather than failing the tick.
fn parse_node_meta(node: &Value) -> NodeMeta {
	let str_at = |ptr: &str| {
		node.pointer(ptr)
			.and_then(Value::as_str)
			.unwrap_or_default()
			.to_string()
	};
	let hostname = {
		let label = node
			.pointer("/metadata/labels/kubernetes.io~1hostname")
			.and_then(Value::as_str)
			.unwrap_or_default();
		if label.is_empty() {
			str_at("/metadata/name")
		} else {
			label.to_string()
		}
	};
	let capacity_cores = node
		.pointer("/status/capacity/cpu")
		.and_then(Value::as_str)
		.and_then(parse_quantity_cores)
		.unwrap_or(0.0);
	let capacity_mem_bytes = node
		.pointer("/status/capacity/memory")
		.and_then(Value::as_str)
		.and_then(parse_quantity_bytes)
		.unwrap_or(0);
	let internal_ip = node
		.pointer("/status/addresses")
		.and_then(Value::as_array)
		.and_then(|addrs| {
			addrs.iter().find_map(|a| {
				(a.get("type").and_then(Value::as_str) == Some("InternalIP"))
					.then(|| a.get("address").and_then(Value::as_str))
					.flatten()
					.map(str::to_string)
			})
		});
	NodeMeta {
		hostname,
		engine_version: str_at("/status/nodeInfo/containerRuntimeVersion"),
		api_version: str_at("/status/nodeInfo/kubeletVersion"),
		kernel_version: str_at("/status/nodeInfo/kernelVersion"),
		capacity_cores,
		capacity_mem_bytes,
		internal_ip,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn node_meta_full_object() {
		let node = json!({
			"metadata": {
				"name": "k3s-1",
				"labels": {"kubernetes.io/hostname": "k3s-1.local"}
			},
			"status": {
				"capacity": {"cpu": "4", "memory": "8131204Ki"},
				"nodeInfo": {
					"containerRuntimeVersion": "containerd://1.7.11-k3s2",
					"kubeletVersion": "v1.29.6+k3s1",
					"kernelVersion": "6.1.0-28-amd64"
				},
				"addresses": [
					{"type": "InternalIP", "address": "10.0.0.5"},
					{"type": "Hostname", "address": "k3s-1"}
				]
			}
		});
		let meta = parse_node_meta(&node);
		assert_eq!(meta.hostname, "k3s-1.local");
		assert_eq!(meta.engine_version, "containerd://1.7.11-k3s2");
		assert_eq!(meta.api_version, "v1.29.6+k3s1");
		assert_eq!(meta.kernel_version, "6.1.0-28-amd64");
		assert_eq!(meta.capacity_cores, 4.0);
		assert_eq!(meta.capacity_mem_bytes, 8_131_204 * 1024);
		assert_eq!(meta.internal_ip.as_deref(), Some("10.0.0.5"));
	}

	#[test]
	fn node_meta_falls_back_to_name_and_zeros() {
		let node = json!({"metadata": {"name": "bare"}});
		let meta = parse_node_meta(&node);
		assert_eq!(meta.hostname, "bare");
		assert_eq!(meta.capacity_cores, 0.0);
		assert_eq!(meta.capacity_mem_bytes, 0);
		assert_eq!(meta.internal_ip, None);
		assert!(meta.engine_version.is_empty());
	}
}
