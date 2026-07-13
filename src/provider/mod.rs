//! Orchestrator abstraction.
//!
//! A [`Provider`] hides the difference between the two supported backends:
//!
//! - [`docker::DockerProvider`] — Docker Engine / Swarm via bollard,
//! - [`kubernetes::KubernetesProvider`] — Kubernetes (k3s) via the API server
//!   and the kubelet Summary API.
//!
//! The shared core (config, [`crate::sink::Sink`], the stats ticker in
//! [`crate::tasks::stats`]) only ever sees this trait.

pub mod docker;
pub mod kubernetes;

use std::sync::Arc;

use crate::models::Status;
use crate::sink::Sink;

/// A source of node/container statistics and orchestrator events.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
	/// Orchestrator identifier put into outbound payloads:
	/// `"swarm"` or `"kubernetes"`.
	fn orchestrator(&self) -> &'static str;

	/// Collects one full [`Status`] snapshot for this node: identity,
	/// host CPU/memory/disk, and per-container statistics.
	///
	/// Implementations fill every field except `agent_version`, which the
	/// caller stamps with the crate version.
	async fn status(&self) -> anyhow::Result<Status>;

	/// Streams orchestrator events to `sink` indefinitely.
	///
	/// Implementations own their reconnect/backoff loop and never return
	/// under normal operation.
	async fn run_events(&self, sink: Arc<Sink>);
}
