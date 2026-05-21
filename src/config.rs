//! Environment-driven configuration for the Swarmboty agent.
//!
//! All settings are read from environment variables at startup.
//! Unset or invalid variables fall back to the documented defaults.

use std::env;
use std::time::Duration;

/// Runtime configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
	/// How often to sample container statistics (minimum 1 s). `STATS_FREQUENCY` env, default 30 s.
	pub stats_frequency: Duration,
	/// URL of the Swarmboty `/events` endpoint. `EVENT_ENDPOINT` env.
	pub event_endpoint: String,
	/// URL used to poll until Swarmboty is ready. `HEALTH_CHECK_ENDPOINT` env.
	pub health_check_endpoint: String,
	/// When `true`, log each forwarded Docker event at `DEBUG` level. `DEBUG_EVENT` env.
	pub debug_event: bool,
	/// When `true`, log each stats payload at `DEBUG` level. `DEBUG_STATS` env.
	pub debug_stats: bool,
	/// Maximum number of concurrent `docker stats` calls per tick. `STATS_MAX_CONCURRENCY` env, default 32, clamped to 1–512.
	pub stats_max_concurrency: usize,
	/// Maximum response size for `GET /logs/:container`. `LOGS_MAX_BYTES` env, default 4 MiB.
	pub logs_max_bytes: usize,
}

impl Config {
	/// Build a [`Config`] from the process environment.
	pub fn from_env() -> Self {
		Self {
			stats_frequency: Duration::from_secs(parse_u64_env("STATS_FREQUENCY", 30).max(1)),
			event_endpoint: get_string("EVENT_ENDPOINT", "http://app:8080/events"),
			health_check_endpoint: get_string("HEALTH_CHECK_ENDPOINT", "http://app:8080/version"),
			debug_event: parse_bool_env("DEBUG_EVENT", false),
			debug_stats: parse_bool_env("DEBUG_STATS", false),
			stats_max_concurrency: parse_usize_env("STATS_MAX_CONCURRENCY", 32).clamp(1, 512),
			logs_max_bytes: parse_usize_env("LOGS_MAX_BYTES", 4 * 1024 * 1024).max(4096),
		}
	}

	/// Returns the subset of config fields exposed by `GET /`.
	pub fn to_info_json(&self) -> InfoResponse {
		InfoResponse {
			stats_frequency: self.stats_frequency.as_secs() as i64,
			event_endpoint: self.event_endpoint.clone(),
			healthcheck_endpoint: self.health_check_endpoint.clone(),
			debug: DebugFlags {
				event: self.debug_event,
				stats: self.debug_stats,
			},
		}
	}
}

/// JSON body returned by `GET /`.
#[derive(Debug, serde::Serialize)]
pub struct InfoResponse {
	/// Stats sampling interval in seconds.
	pub stats_frequency: i64,
	/// Target URL for event forwarding.
	pub event_endpoint: String,
	/// URL polled to detect Swarmboty readiness.
	pub healthcheck_endpoint: String,
	/// Active debug flags.
	pub debug: DebugFlags,
}

/// Debug logging flags included in the info response.
#[derive(Debug, serde::Serialize)]
pub struct DebugFlags {
	/// `true` when Docker event payloads are logged at `DEBUG` level.
	pub event: bool,
	/// `true` when stats payloads are logged at `DEBUG` level.
	pub stats: bool,
}

fn get_string(key: &str, default: &str) -> String {
	match env::var(key) {
		Ok(v) if !v.is_empty() => v,
		_ => default.to_string(),
	}
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
		env::set_var("LOGS_MAX_BYTES", "1");
		let cfg = Config::from_env();
		assert_eq!(cfg.stats_frequency.as_secs(), 1);
		assert_eq!(cfg.stats_max_concurrency, 512);
		assert_eq!(cfg.logs_max_bytes, 4096);
		env::remove_var("STATS_FREQUENCY");
		env::remove_var("STATS_MAX_CONCURRENCY");
		env::remove_var("LOGS_MAX_BYTES");
	}

	#[test]
	fn to_info_json_reflects_config() {
		let cfg = Config {
			stats_frequency: Duration::from_secs(42),
			event_endpoint: "http://events".into(),
			health_check_endpoint: "http://health".into(),
			debug_event: true,
			debug_stats: false,
			stats_max_concurrency: 16,
			logs_max_bytes: 8192,
		};
		let info = cfg.to_info_json();
		assert_eq!(info.stats_frequency, 42);
		assert_eq!(info.event_endpoint, "http://events");
		assert_eq!(info.healthcheck_endpoint, "http://health");
		assert!(info.debug.event);
		assert!(!info.debug.stats);
	}
}
