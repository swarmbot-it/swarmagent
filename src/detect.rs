//! Orchestrator auto-detection.
//!
//! The agent decides at startup whether it runs inside a Kubernetes cluster
//! (k3s or any conformant distribution) or next to a Docker Engine (Swarm or
//! standalone). An explicit `AGENT_MODE` always wins; in `auto` mode the
//! in-cluster ServiceAccount markers are checked first, because on k3s the
//! Docker socket usually does not exist while the `KUBERNETES_*` environment
//! is always injected into pods.

use std::path::Path;

use crate::config::AgentMode;

/// Path of the in-cluster ServiceAccount token mounted into every pod.
pub const SERVICEACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// Resolved orchestrator mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
	/// Talk to a Docker Engine (Swarm node or standalone daemon).
	Docker,
	/// Talk to the Kubernetes API server and kubelet.
	Kubernetes,
}

impl Mode {
	/// Human-readable name used in logs.
	pub fn as_str(&self) -> &'static str {
		match self {
			Mode::Docker => "docker",
			Mode::Kubernetes => "kubernetes",
		}
	}
}

/// Returns `true` when the process appears to run inside a Kubernetes pod
/// (ServiceAccount token mounted and API server env variables injected).
pub fn in_cluster() -> bool {
	let token = Path::new(SERVICEACCOUNT_DIR).join("token");
	token.is_file() && std::env::var("KUBERNETES_SERVICE_HOST").is_ok_and(|v| !v.is_empty())
}

/// Resolves the requested [`AgentMode`] to a concrete [`Mode`].
///
/// `kubernetes_detected` is the result of [`in_cluster`]; it is a parameter
/// so the decision table stays unit-testable without touching the process
/// environment or filesystem.
pub fn resolve(requested: AgentMode, kubernetes_detected: bool) -> Mode {
	match requested {
		AgentMode::Docker => Mode::Docker,
		AgentMode::Kubernetes => Mode::Kubernetes,
		AgentMode::Auto => {
			if kubernetes_detected {
				Mode::Kubernetes
			} else {
				Mode::Docker
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn explicit_mode_wins_over_detection() {
		assert_eq!(resolve(AgentMode::Docker, true), Mode::Docker);
		assert_eq!(resolve(AgentMode::Kubernetes, false), Mode::Kubernetes);
	}

	#[test]
	fn auto_follows_detection() {
		assert_eq!(resolve(AgentMode::Auto, true), Mode::Kubernetes);
		assert_eq!(resolve(AgentMode::Auto, false), Mode::Docker);
	}

	#[test]
	fn mode_names() {
		assert_eq!(Mode::Docker.as_str(), "docker");
		assert_eq!(Mode::Kubernetes.as_str(), "kubernetes");
	}
}
