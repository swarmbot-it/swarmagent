//! Environment-driven configuration for the Swarmbot agent.
//!
//! All settings are read from environment variables at startup.
//! Unset or invalid variables fall back to the documented defaults.

use std::env;
use std::time::Duration;

/// Requested agent mode (`AGENT_MODE` env). `Auto` resolves at startup based
/// on the environment the agent runs in — see [`crate::detect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
	/// Detect the orchestrator automatically (default).
	Auto,
	/// Force the Docker Engine provider (Swarm or standalone).
	Docker,
	/// Force the Kubernetes provider (k3s or any conformant cluster).
	Kubernetes,
}

/// How container/node statistics are fetched in Kubernetes mode
/// (`AGENT_KUBELET_MODE` env).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KubeletMode {
	/// Query the kubelet Summary API directly on `https://<hostIP>:10250`.
	/// Falls back to `Proxy` when the direct call fails.
	Direct,
	/// Always go through the API server proxy
	/// (`/api/v1/nodes/{node}/proxy/stats/summary`).
	Proxy,
}

/// Runtime configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
	/// Requested orchestrator mode. `AGENT_MODE` env, default `auto`.
	pub agent_mode: AgentMode,
	/// How often to sample container statistics (minimum 1 s). `STATS_FREQUENCY` env, default 30 s.
	pub stats_frequency: Duration,
	/// URL of the Swarmbot `/events` endpoint. `EVENT_ENDPOINT` env.
	pub event_endpoint: String,
	/// URL used to poll until Swarmbot is ready. `HEALTH_CHECK_ENDPOINT` env.
	pub health_check_endpoint: String,
	/// When `true`, log each forwarded Docker event at `DEBUG` level. `DEBUG_EVENT` env.
	pub debug_event: bool,
	/// When `true`, log each stats payload at `DEBUG` level. `DEBUG_STATS` env.
	pub debug_stats: bool,
	/// Maximum number of concurrent `docker stats` calls per tick (Docker mode only).
	/// `STATS_MAX_CONCURRENCY` env, default 32, clamped to 1–512.
	pub stats_max_concurrency: usize,
	/// Kubernetes node this agent runs on. `NODE_NAME` env (Downward API,
	/// `fieldRef: spec.nodeName`). Required in Kubernetes mode.
	pub node_name: Option<String>,
	/// Host IP of the node for direct kubelet access. `NODE_IP` env (Downward
	/// API, `fieldRef: status.hostIP`). Optional — resolved from the Node
	/// object when unset.
	pub node_ip: Option<String>,
	/// Skip TLS verification when talking to the kubelet directly (k3s uses a
	/// self-signed serving certificate). `AGENT_KUBELET_INSECURE_TLS` env, default `true`.
	pub kubelet_insecure_tls: bool,
	/// Kubelet access mode. `AGENT_KUBELET_MODE` env (`direct`/`proxy`), default `direct`.
	pub kubelet_mode: KubeletMode,
	/// Shared secret sent as `X-Agent-Token` on every request to Swarmbot.
	/// `SWARMAGENT_SHARED_SECRET` env. Opt-in: unset means no token is sent,
	/// matching the previous behavior.
	pub shared_secret: Option<String>,
}

impl Config {
	/// Build a [`Config`] from the process environment.
	pub fn from_env() -> Self {
		let base = swarmbot_base_url();
		Self {
			agent_mode: parse_agent_mode(&get_string("AGENT_MODE", "auto")),
			stats_frequency: Duration::from_secs(parse_u64_env("STATS_FREQUENCY", 30).max(1)),
			event_endpoint: get_string("EVENT_ENDPOINT", &endpoint_from_base(&base, "/events")),
			health_check_endpoint: get_string(
				"HEALTH_CHECK_ENDPOINT",
				&endpoint_from_base(&base, "/version"),
			),
			debug_event: parse_bool_env("DEBUG_EVENT", false),
			debug_stats: parse_bool_env("DEBUG_STATS", false),
			stats_max_concurrency: parse_usize_env("STATS_MAX_CONCURRENCY", 32).clamp(1, 512),
			node_name: non_empty(env::var("NODE_NAME").ok()),
			node_ip: non_empty(env::var("NODE_IP").ok()),
			kubelet_insecure_tls: parse_bool_env("AGENT_KUBELET_INSECURE_TLS", true),
			kubelet_mode: parse_kubelet_mode(&get_string("AGENT_KUBELET_MODE", "direct")),
			shared_secret: non_empty(env::var("SWARMAGENT_SHARED_SECRET").ok()),
		}
	}
}

fn non_empty(v: Option<String>) -> Option<String> {
	v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn parse_agent_mode(raw: &str) -> AgentMode {
	match raw.trim().to_ascii_lowercase().as_str() {
		"docker" | "swarm" => AgentMode::Docker,
		"kubernetes" | "k8s" | "k3s" => AgentMode::Kubernetes,
		_ => AgentMode::Auto,
	}
}

fn parse_kubelet_mode(raw: &str) -> KubeletMode {
	match raw.trim().to_ascii_lowercase().as_str() {
		"proxy" => KubeletMode::Proxy,
		_ => KubeletMode::Direct,
	}
}

fn get_string(key: &str, default: &str) -> String {
	match env::var(key) {
		Ok(v) if !v.is_empty() => v,
		_ => default.to_string(),
	}
}

/// Base URL of the Swarmbot app.
///
/// Read from `SWARMBOT_URL` (name used by the swarmbot compose files);
/// otherwise derived from `EVENT_ENDPOINT`.
fn swarmbot_base_url() -> String {
	let direct = get_string("SWARMBOT_URL", "");
	if !direct.is_empty() {
		return trim_trailing_slash(&direct);
	}
	let event = get_string("EVENT_ENDPOINT", "http://app:8080/events");
	let base = event.strip_suffix("/events").unwrap_or(event.as_str());
	trim_trailing_slash(base)
}

fn trim_trailing_slash(url: &str) -> String {
	url.trim_end_matches('/').to_string()
}

fn endpoint_from_base(base: &str, path: &str) -> String {
	format!("{base}{path}")
}

fn parse_bool_env(key: &str, default: bool) -> bool {
	env::var(key)
		.ok()
		.and_then(|v| v.parse().ok())
		.unwrap_or(default)
}

fn parse_u64_env(key: &str, default: u64) -> u64 {
	env::var(key)
		.ok()
		.and_then(|v| v.parse().ok())
		.unwrap_or(default)
}

fn parse_usize_env(key: &str, default: usize) -> usize {
	env::var(key)
		.ok()
		.and_then(|v| v.parse().ok())
		.unwrap_or(default)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{Mutex, MutexGuard};

	fn env_lock() -> MutexGuard<'static, ()> {
		static LOCK: Mutex<()> = Mutex::new(());
		LOCK.lock().unwrap()
	}

	#[test]
	fn parse_bool_uses_default_when_unset() {
		assert!(parse_bool_env("SWARMagent_TEST_BOOL_UNSET", true));
		assert!(!parse_bool_env("SWARMagent_TEST_BOOL_UNSET", false));
	}

	#[test]
	fn parse_numeric_env_uses_default_on_invalid() {
		assert_eq!(parse_u64_env("SWARMagent_TEST_U64_UNSET", 7), 7);
		assert_eq!(parse_usize_env("SWARMagent_TEST_USIZE_UNSET", 9), 9);
	}

	#[test]
	fn get_string_empty_falls_back() {
		let _guard = env_lock();
		let key = "SWARMagent_TEST_EMPTY_STRING";
		env::set_var(key, "");
		assert_eq!(get_string(key, "default"), "default");
		env::remove_var(key);
	}

	#[test]
	fn from_env_clamps_and_minimums() {
		let _guard = env_lock();
		env::set_var("STATS_FREQUENCY", "0");
		env::set_var("STATS_MAX_CONCURRENCY", "9999");
		let cfg = Config::from_env();
		assert_eq!(cfg.stats_frequency.as_secs(), 1);
		assert_eq!(cfg.stats_max_concurrency, 512);
		env::remove_var("STATS_FREQUENCY");
		env::remove_var("STATS_MAX_CONCURRENCY");
	}

	#[test]
	fn agent_mode_parsing() {
		assert_eq!(parse_agent_mode("auto"), AgentMode::Auto);
		assert_eq!(parse_agent_mode("Docker"), AgentMode::Docker);
		assert_eq!(parse_agent_mode("swarm"), AgentMode::Docker);
		assert_eq!(parse_agent_mode("kubernetes"), AgentMode::Kubernetes);
		assert_eq!(parse_agent_mode("K3S"), AgentMode::Kubernetes);
		assert_eq!(parse_agent_mode("nonsense"), AgentMode::Auto);
	}

	#[test]
	fn kubelet_mode_parsing() {
		assert_eq!(parse_kubelet_mode("proxy"), KubeletMode::Proxy);
		assert_eq!(parse_kubelet_mode("direct"), KubeletMode::Direct);
		assert_eq!(parse_kubelet_mode(""), KubeletMode::Direct);
	}

	#[test]
	fn base_url_reads_swarmbot_url() {
		let _guard = env_lock();
		env::set_var("SWARMBOT_URL", "http://swarmbot:6666/");
		let cfg = Config::from_env();
		assert_eq!(cfg.event_endpoint, "http://swarmbot:6666/events");
		assert_eq!(cfg.health_check_endpoint, "http://swarmbot:6666/version");
		env::remove_var("SWARMBOT_URL");
	}

	#[test]
	fn non_empty_trims_and_filters() {
		assert_eq!(non_empty(Some("  ".into())), None);
		assert_eq!(non_empty(Some(" x ".into())), Some("x".to_string()));
		assert_eq!(non_empty(None), None);
	}
}
