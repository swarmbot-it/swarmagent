//! Environment-driven configuration for the Swarmbot agent.

use std::env;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub stats_frequency: Duration,
    pub event_endpoint: String,
    pub health_check_endpoint: String,
    pub debug_event: bool,
    pub debug_stats: bool,
    pub stats_max_concurrency: usize,
    pub logs_max_bytes: usize,
    /// Shared secret sent as `X-Agent-Token` to Swarmbot and required (if set)
    /// from callers of this agent's own HTTP API. Opt-in: unset means no
    /// auth is enforced, matching the previous behavior.
    pub shared_secret: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            stats_frequency: Duration::from_secs(parse_u64_env("STATS_FREQUENCY", 30).max(1)),
            event_endpoint: get_string("EVENT_ENDPOINT", "http://app:8080/events"),
            health_check_endpoint: get_string("HEALTH_CHECK_ENDPOINT", "http://app:8080/version"),
            debug_event: parse_bool_env("DEBUG_EVENT", false),
            debug_stats: parse_bool_env("DEBUG_STATS", false),
            stats_max_concurrency: parse_usize_env("STATS_MAX_CONCURRENCY", 32).clamp(1, 512),
            logs_max_bytes: parse_usize_env("LOGS_MAX_BYTES", 4 * 1024 * 1024).max(4096),
            shared_secret: env::var("SWARMAGENT_SHARED_SECRET")
                .ok()
                .filter(|v| !v.is_empty()),
        }
    }

    /// Public JSON view for `GET /`.
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

#[derive(Debug, serde::Serialize)]
pub struct InfoResponse {
    pub stats_frequency: i64,
    pub event_endpoint: String,
    pub healthcheck_endpoint: String,
    pub debug: DebugFlags,
}

#[derive(Debug, serde::Serialize)]
pub struct DebugFlags {
    pub event: bool,
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
