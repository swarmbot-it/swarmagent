//! Pod metadata extraction and Kubernetes quantity parsing.
//!
//! The stats tick lists the pods scheduled on this node once and derives,
//! per pod: memory limits per container (for `memoryPercentage`) and the
//! owning workload (Deployment/StatefulSet/DaemonSet/Job/…) reported to
//! Swarmboty as `workload`/`workloadKind`.

use std::collections::HashMap;

use serde_json::Value;

/// Per-pod metadata derived from the pod list.
#[derive(Debug, Default, Clone)]
pub struct PodMeta {
	/// Owning workload name (e.g. Deployment name), when resolvable.
	pub workload: Option<String>,
	/// Owning workload kind (`Deployment`, `StatefulSet`, `DaemonSet`, `Job`, …).
	pub workload_kind: Option<String>,
	/// Memory limit in bytes per container name (absent = unlimited).
	pub mem_limits: HashMap<String, u64>,
}

/// Parses a Kubernetes resource quantity into bytes (for memory-like values).
///
/// Supports plain integers/decimals, binary suffixes (`Ki`, `Mi`, `Gi`, `Ti`,
/// `Pi`, `Ei`), decimal suffixes (`k`, `M`, `G`, `T`, `P`, `E`) and the
/// milli suffix (`m`). Returns `None` for malformed input.
pub fn parse_quantity_bytes(raw: &str) -> Option<u64> {
	let (value, multiplier) = split_quantity(raw)?;
	let bytes = value * multiplier;
	if bytes.is_finite() && bytes >= 0.0 {
		Some(bytes.round() as u64)
	} else {
		None
	}
}

/// Parses a CPU quantity into a number of cores (`"4"` → 4.0, `"3800m"` → 3.8).
pub fn parse_quantity_cores(raw: &str) -> Option<f64> {
	let (value, multiplier) = split_quantity(raw)?;
	let cores = value * multiplier;
	if cores.is_finite() && cores >= 0.0 {
		Some(cores)
	} else {
		None
	}
}

/// Splits `"128Mi"` into `(128.0, 1024*1024)`. Shared by both parsers.
fn split_quantity(raw: &str) -> Option<(f64, f64)> {
	let s = raw.trim();
	if s.is_empty() {
		return None;
	}
	let split = s
		.find(|c: char| {
			!(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')
		})
		.unwrap_or(s.len());
	// A trailing `E` is the decimal exa suffix, not an exponent marker.
	let split = if split == s.len() && s.ends_with(['e', 'E']) {
		s.len() - 1
	} else {
		split
	};
	let (num, suffix) = s.split_at(split);
	let value: f64 = num.parse().ok()?;
	let multiplier: f64 = match suffix {
		"" => 1.0,
		"m" => 1e-3,
		"k" => 1e3,
		"M" => 1e6,
		"G" => 1e9,
		"T" => 1e12,
		"P" => 1e15,
		"E" | "e" => 1e18,
		"Ki" => 1024.0,
		"Mi" => 1024.0 * 1024.0,
		"Gi" => 1024.0 * 1024.0 * 1024.0,
		"Ti" => 1024f64.powi(4),
		"Pi" => 1024f64.powi(5),
		"Ei" => 1024f64.powi(6),
		_ => return None,
	};
	Some((value, multiplier))
}

/// Maps a pod `ownerReference` to the workload shown in Swarmboty.
///
/// Pods owned by a ReplicaSet are reported as their Deployment: the
/// ReplicaSet name is `<deployment>-<pod-template-hash>`, so the last
/// `-`-separated segment is stripped. This avoids one extra API call per
/// pod; the heuristic only misfires for a bare ReplicaSet whose own name
/// ends in a dash-suffix, which is rare enough to accept.
pub fn workload_from_owner(kind: &str, name: &str) -> (String, String) {
	if kind == "ReplicaSet" {
		if let Some((prefix, _hash)) = name.rsplit_once('-') {
			if !prefix.is_empty() {
				return ("Deployment".to_string(), prefix.to_string());
			}
		}
	}
	(kind.to_string(), name.to_string())
}

/// Extracts [`PodMeta`] entries from a `PodList` JSON object, keyed by
/// `"{namespace}/{pod}"`.
pub fn parse_pod_metas(pod_list: &Value) -> HashMap<String, PodMeta> {
	let mut out = HashMap::new();
	let Some(items) = pod_list.get("items").and_then(Value::as_array) else {
		return out;
	};
	for item in items {
		let namespace = item
			.pointer("/metadata/namespace")
			.and_then(Value::as_str)
			.unwrap_or_default();
		let name = item
			.pointer("/metadata/name")
			.and_then(Value::as_str)
			.unwrap_or_default();
		if namespace.is_empty() || name.is_empty() {
			continue;
		}

		let mut meta = PodMeta::default();

		if let Some(owner) = controller_owner(item) {
			let kind = owner
				.get("kind")
				.and_then(Value::as_str)
				.unwrap_or_default();
			let owner_name = owner
				.get("name")
				.and_then(Value::as_str)
				.unwrap_or_default();
			if !kind.is_empty() && !owner_name.is_empty() {
				let (workload_kind, workload) = workload_from_owner(kind, owner_name);
				meta.workload = Some(workload);
				meta.workload_kind = Some(workload_kind);
			}
		}

		if let Some(containers) = item.pointer("/spec/containers").and_then(Value::as_array) {
			for c in containers {
				let cname = c.get("name").and_then(Value::as_str).unwrap_or_default();
				let limit = c
					.pointer("/resources/limits/memory")
					.and_then(Value::as_str)
					.and_then(parse_quantity_bytes);
				if let (false, Some(bytes)) = (cname.is_empty(), limit) {
					meta.mem_limits.insert(cname.to_string(), bytes);
				}
			}
		}

		out.insert(format!("{namespace}/{name}"), meta);
	}
	out
}

/// Returns the controlling `ownerReference` of a pod (falls back to the
/// first owner when none is marked `controller: true`).
fn controller_owner(pod: &Value) -> Option<&Value> {
	let owners = pod
		.pointer("/metadata/ownerReferences")
		.and_then(Value::as_array)?;
	owners
		.iter()
		.find(|o| o.get("controller").and_then(Value::as_bool) == Some(true))
		.or_else(|| owners.first())
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn quantity_bytes_plain_and_binary() {
		assert_eq!(parse_quantity_bytes("128974848"), Some(128_974_848));
		assert_eq!(parse_quantity_bytes("1Ki"), Some(1024));
		assert_eq!(parse_quantity_bytes("128Mi"), Some(128 * 1024 * 1024));
		assert_eq!(parse_quantity_bytes("2Gi"), Some(2 * 1024 * 1024 * 1024));
	}

	#[test]
	fn quantity_bytes_decimal_suffixes() {
		assert_eq!(parse_quantity_bytes("1k"), Some(1000));
		assert_eq!(parse_quantity_bytes("1M"), Some(1_000_000));
		assert_eq!(parse_quantity_bytes("1500m"), Some(2)); // 1.5 rounded
	}

	#[test]
	fn quantity_bytes_rejects_garbage() {
		assert_eq!(parse_quantity_bytes(""), None);
		assert_eq!(parse_quantity_bytes("abc"), None);
		assert_eq!(parse_quantity_bytes("12Xi"), None);
		assert_eq!(parse_quantity_bytes("-5"), None);
	}

	#[test]
	fn quantity_cores() {
		assert_eq!(parse_quantity_cores("4"), Some(4.0));
		assert!((parse_quantity_cores("3800m").unwrap() - 3.8).abs() < 1e-9);
		assert!((parse_quantity_cores("250m").unwrap() - 0.25).abs() < 1e-9);
	}

	#[test]
	fn workload_replicaset_maps_to_deployment() {
		assert_eq!(
			workload_from_owner("ReplicaSet", "web-7f9c6b7d54"),
			("Deployment".to_string(), "web".to_string())
		);
		assert_eq!(
			workload_from_owner("ReplicaSet", "my-app-name-7f9c6b7d54"),
			("Deployment".to_string(), "my-app-name".to_string())
		);
	}

	#[test]
	fn workload_other_kinds_pass_through() {
		assert_eq!(
			workload_from_owner("StatefulSet", "db"),
			("StatefulSet".to_string(), "db".to_string())
		);
		assert_eq!(
			workload_from_owner("DaemonSet", "swarmagent"),
			("DaemonSet".to_string(), "swarmagent".to_string())
		);
	}

	#[test]
	fn parse_pod_metas_extracts_owner_and_limits() {
		let list = json!({
			"items": [{
				"metadata": {
					"namespace": "prod",
					"name": "web-7f9c6b7d54-abcde",
					"ownerReferences": [
						{"kind": "ReplicaSet", "name": "web-7f9c6b7d54", "controller": true}
					]
				},
				"spec": {
					"containers": [
						{"name": "web", "resources": {"limits": {"memory": "256Mi"}}},
						{"name": "sidecar", "resources": {}}
					]
				}
			}]
		});
		let metas = parse_pod_metas(&list);
		let m = metas.get("prod/web-7f9c6b7d54-abcde").expect("pod meta");
		assert_eq!(m.workload.as_deref(), Some("web"));
		assert_eq!(m.workload_kind.as_deref(), Some("Deployment"));
		assert_eq!(m.mem_limits.get("web"), Some(&(256 * 1024 * 1024)));
		assert!(!m.mem_limits.contains_key("sidecar"));
	}

	#[test]
	fn parse_pod_metas_tolerates_missing_fields() {
		assert!(parse_pod_metas(&json!({})).is_empty());
		let list = json!({"items": [{"metadata": {"namespace": "", "name": "x"}}]});
		assert!(parse_pod_metas(&list).is_empty());
		let list = json!({"items": [{"metadata": {"namespace": "default", "name": "bare-pod"}}]});
		let metas = parse_pod_metas(&list);
		let m = metas.get("default/bare-pod").expect("bare pod");
		assert!(m.workload.is_none());
		assert!(m.mem_limits.is_empty());
	}
}
