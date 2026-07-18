//! Kubernetes event source — a pod watch scoped to this node.
//!
//! Instead of the cluster-wide `v1.Event` feed (which cannot be filtered by
//! node and would be duplicated by every DaemonSet replica), the agent
//! watches the pods scheduled on its own node and derives container
//! lifecycle events from `status.containerStatuses` transitions:
//!
//! | Transition | Emitted action |
//! |---|---|
//! | container becomes running | `start` |
//! | running → terminated | `die` (plus `oom` when `reason == OOMKilled`) |
//! | `restartCount` increases | `die` + `start` |
//! | pod deleted | `destroy` per tracked container |
//!
//! The emitted JSON mirrors the Docker event envelope (`Type`, `Action`,
//! `Actor`, `time`) with an added `orchestrator` marker, so the swarmbot
//! ingest can treat both sources uniformly.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::Value;

/// Container lifecycle event pushed to Swarmbot (Docker-envelope-compatible).
#[derive(Debug, Serialize)]
pub struct AgentEvent {
	/// Always `"container"`.
	#[serde(rename = "Type")]
	pub typ: &'static str,
	/// `start`, `die`, `oom`, or `destroy`.
	#[serde(rename = "Action")]
	pub action: &'static str,
	/// Always `"kubernetes"`.
	pub orchestrator: &'static str,
	/// Event subject.
	#[serde(rename = "Actor")]
	pub actor: Actor,
	/// Unix timestamp in seconds.
	pub time: i64,
}

/// Subject of an [`AgentEvent`].
#[derive(Debug, Serialize)]
pub struct Actor {
	/// `"{namespace}/{pod}/{container}"` — matches the stats container id.
	#[serde(rename = "ID")]
	pub id: String,
	/// Free-form metadata: `name`, `namespace`, `pod`, `exitCode`, `reason`, …
	#[serde(rename = "Attributes")]
	pub attributes: BTreeMap<String, String>,
}

/// Last observed state of one container, keyed by its stats id.
#[derive(Debug, Default, Clone, Copy)]
pub struct TrackedContainer {
	running: bool,
	restart_count: i64,
}

/// Watch-level container state, persisted across watch reconnects.
pub type WatchState = HashMap<String, TrackedContainer>;

fn ts_or(now: i64, rfc3339: Option<&str>) -> i64 {
	rfc3339
		.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
		.map(|d| d.timestamp())
		.unwrap_or(now)
}

fn base_attributes(namespace: &str, pod: &str, container: &str) -> BTreeMap<String, String> {
	BTreeMap::from([
		("name".to_string(), container.to_string()),
		("namespace".to_string(), namespace.to_string()),
		("pod".to_string(), pod.to_string()),
	])
}

fn event(
	action: &'static str,
	id: &str,
	attributes: BTreeMap<String, String>,
	time: i64,
) -> AgentEvent {
	AgentEvent {
		typ: "container",
		action,
		orchestrator: "kubernetes",
		actor: Actor {
			id: id.to_string(),
			attributes,
		},
		time,
	}
}

/// Applies one watch event (`{"type": "...", "object": <Pod>}`) to `state`
/// and returns the lifecycle events to forward.
///
/// `now` is the fallback timestamp (unix seconds) for events whose pod status
/// carries no usable timestamp. Pure function — unit-tested without a cluster.
pub fn diff_watch_event(
	state: &mut WatchState,
	event_type: &str,
	pod: &Value,
	now: i64,
) -> Vec<AgentEvent> {
	let namespace = pod
		.pointer("/metadata/namespace")
		.and_then(Value::as_str)
		.unwrap_or_default();
	let pod_name = pod
		.pointer("/metadata/name")
		.and_then(Value::as_str)
		.unwrap_or_default();
	if namespace.is_empty() || pod_name.is_empty() {
		return Vec::new();
	}
	let prefix = format!("{namespace}/{pod_name}/");

	if event_type == "DELETED" {
		let mut out = Vec::new();
		let keys: Vec<String> = state
			.keys()
			.filter(|k| k.starts_with(&prefix))
			.cloned()
			.collect();
		for key in keys {
			state.remove(&key);
			let container = key.rsplit('/').next().unwrap_or_default().to_string();
			out.push(event(
				"destroy",
				&key,
				base_attributes(namespace, pod_name, &container),
				now,
			));
		}
		return out;
	}

	let Some(statuses) = pod
		.pointer("/status/containerStatuses")
		.and_then(Value::as_array)
	else {
		return Vec::new();
	};

	let mut out = Vec::new();
	for cs in statuses {
		let container = cs.get("name").and_then(Value::as_str).unwrap_or_default();
		if container.is_empty() {
			continue;
		}
		let key = format!("{prefix}{container}");
		let restart_count = cs.get("restartCount").and_then(Value::as_i64).unwrap_or(0);
		let running = cs.pointer("/state/running").is_some();
		let terminated = cs.pointer("/state/terminated");
		let prev = state.get(&key).copied();

		// A restart while we were not watching shows up only as a counter bump.
		let missed_restart = prev.is_some_and(|p| restart_count > p.restart_count);

		if let Some(term) = terminated {
			let was_running = prev.is_some_and(|p| p.running);
			if was_running || missed_restart {
				let reason = term.get("reason").and_then(Value::as_str).unwrap_or("");
				let time = ts_or(now, term.get("finishedAt").and_then(Value::as_str));
				let mut attrs = base_attributes(namespace, pod_name, container);
				if let Some(code) = term.get("exitCode").and_then(Value::as_i64) {
					attrs.insert("exitCode".to_string(), code.to_string());
				}
				if !reason.is_empty() {
					attrs.insert("reason".to_string(), reason.to_string());
				}
				if reason == "OOMKilled" {
					out.push(event("oom", &key, attrs.clone(), time));
				}
				out.push(event("die", &key, attrs, time));
			}
		}

		if running {
			let was_running = prev.is_some_and(|p| p.running);
			if !was_running || missed_restart {
				if missed_restart && prev.is_some_and(|p| p.running) {
					// running → running with a restart in between: emit the
					// die we missed before the start.
					out.push(event(
						"die",
						&key,
						base_attributes(namespace, pod_name, container),
						now,
					));
				}
				let time = ts_or(
					now,
					cs.pointer("/state/running/startedAt")
						.and_then(Value::as_str),
				);
				out.push(event(
					"start",
					&key,
					base_attributes(namespace, pod_name, container),
					time,
				));
			}
		}

		state.insert(
			key,
			TrackedContainer {
				running,
				restart_count,
			},
		);
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	fn pod(container_state: Value, restart_count: i64) -> Value {
		json!({
			"metadata": {"namespace": "prod", "name": "web-1"},
			"status": {"containerStatuses": [{
				"name": "web",
				"restartCount": restart_count,
				"state": container_state
			}]}
		})
	}

	#[test]
	fn start_emitted_once() {
		let mut state = WatchState::new();
		let p = pod(json!({"running": {"startedAt": "2026-07-13T10:00:00Z"}}), 0);

		let events = diff_watch_event(&mut state, "MODIFIED", &p, 111);
		assert_eq!(events.len(), 1);
		assert_eq!(events[0].action, "start");
		assert_eq!(events[0].actor.id, "prod/web-1/web");
		assert_eq!(events[0].time, 1_783_936_800); // 2026-07-13T10:00:00Z

		// Same state again → no duplicate events.
		let events = diff_watch_event(&mut state, "MODIFIED", &p, 112);
		assert!(events.is_empty());
	}

	#[test]
	fn running_to_terminated_emits_die() {
		let mut state = WatchState::new();
		diff_watch_event(&mut state, "ADDED", &pod(json!({"running": {}}), 0), 100);

		let terminated = pod(
			json!({"terminated": {"exitCode": 1, "reason": "Error", "finishedAt": "2026-07-13T10:05:00Z"}}),
			0,
		);
		let events = diff_watch_event(&mut state, "MODIFIED", &terminated, 200);
		assert_eq!(events.len(), 1);
		assert_eq!(events[0].action, "die");
		assert_eq!(events[0].actor.attributes.get("exitCode").unwrap(), "1");
		assert_eq!(events[0].actor.attributes.get("reason").unwrap(), "Error");
	}

	#[test]
	fn oom_kill_emits_oom_and_die() {
		let mut state = WatchState::new();
		diff_watch_event(&mut state, "ADDED", &pod(json!({"running": {}}), 0), 100);

		let oom = pod(
			json!({"terminated": {"exitCode": 137, "reason": "OOMKilled"}}),
			0,
		);
		let actions: Vec<&str> = diff_watch_event(&mut state, "MODIFIED", &oom, 200)
			.iter()
			.map(|e| e.action)
			.collect();
		assert_eq!(actions, vec!["oom", "die"]);
	}

	#[test]
	fn missed_restart_emits_die_then_start() {
		let mut state = WatchState::new();
		diff_watch_event(&mut state, "ADDED", &pod(json!({"running": {}}), 0), 100);

		// Watch reconnected after a crash-loop iteration: still running,
		// but restartCount bumped.
		let restarted = pod(json!({"running": {"startedAt": "2026-07-13T11:00:00Z"}}), 1);
		let actions: Vec<&str> = diff_watch_event(&mut state, "MODIFIED", &restarted, 200)
			.iter()
			.map(|e| e.action)
			.collect();
		assert_eq!(actions, vec!["die", "start"]);
	}

	#[test]
	fn delete_emits_destroy_and_clears_state() {
		let mut state = WatchState::new();
		diff_watch_event(&mut state, "ADDED", &pod(json!({"running": {}}), 0), 100);
		assert_eq!(state.len(), 1);

		let deleted = pod(json!({"running": {}}), 0);
		let events = diff_watch_event(&mut state, "DELETED", &deleted, 300);
		assert_eq!(events.len(), 1);
		assert_eq!(events[0].action, "destroy");
		assert_eq!(events[0].actor.id, "prod/web-1/web");
		assert!(state.is_empty());
	}

	#[test]
	fn pending_pod_without_statuses_is_ignored() {
		let mut state = WatchState::new();
		let pending = json!({
			"metadata": {"namespace": "prod", "name": "web-1"},
			"status": {"phase": "Pending"}
		});
		assert!(diff_watch_event(&mut state, "ADDED", &pending, 1).is_empty());
		assert!(state.is_empty());
	}

	#[test]
	fn event_serializes_docker_like_envelope() {
		let e = event("start", "ns/p/c", base_attributes("ns", "p", "c"), 42);
		let v = serde_json::to_value(&e).unwrap();
		assert_eq!(v["Type"], "container");
		assert_eq!(v["Action"], "start");
		assert_eq!(v["orchestrator"], "kubernetes");
		assert_eq!(v["Actor"]["ID"], "ns/p/c");
		assert_eq!(v["Actor"]["Attributes"]["namespace"], "ns");
		assert_eq!(v["time"], 42);
	}
}
