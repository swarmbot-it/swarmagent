//! JSON payloads sent to Swarmboty.

use serde::Serialize;

/// Full status snapshot posted to Swarmboty on every stats tick.
#[derive(Debug, Serialize)]
pub struct Status {
	/// Docker Swarm node ID (empty string when not in Swarm mode).
	pub id: String,
	/// Root filesystem usage.
	pub disk: DiskStatus,
	/// Host CPU usage.
	pub cpu: CpuStatus,
	/// Host memory usage.
	pub memory: MemoryStatus,
	/// Per-container resource snapshots for all running containers.
	pub tasks: Vec<ContainerStatus>,
	/// Docker Engine version string (e.g. `"27.3.1"`).
	#[serde(rename = "engineVersion")]
	pub engine_version: String,
	/// Docker API version the daemon advertises (e.g. `"1.47"`).
	#[serde(rename = "apiVersion")]
	pub api_version: String,
	/// Host kernel version string (e.g. `"6.1.0-28-amd64"`).
	#[serde(rename = "kernelVersion")]
	pub kernel_version: String,
}

/// Root filesystem capacity and usage in bytes.
#[derive(Debug, Default, Serialize)]
pub struct DiskStatus {
	/// Total capacity in bytes.
	pub total: u64,
	/// Used space in bytes.
	pub used: u64,
	/// Used space as a percentage of total (0–100).
	pub used_percentage: f64,
	/// Available space in bytes.
	pub free: u64,
}

/// Host CPU utilisation.
#[derive(Debug, Serialize)]
pub struct CpuStatus {
	/// Current global CPU usage percentage (0–100).
	pub used_percentage: f64,
	/// Number of logical CPU cores as reported by `docker info`.
	pub cores: i32,
}

/// Host memory usage in bytes.
#[derive(Debug, Serialize)]
pub struct MemoryStatus {
	/// Total physical memory.
	pub total: u64,
	/// Used memory (total − free).
	pub used: u64,
	/// Used memory as a percentage of total (0–100).
	pub used_percentage: f64,
	/// Free (unused) memory.
	pub free: u64,
}

/// Resource snapshot for a single running container.
#[derive(Debug, Serialize)]
pub struct ContainerStatus {
	/// Container name as reported by Docker (includes leading `/`).
	pub name: String,
	/// Full container ID.
	pub id: String,
	/// CPU usage percentage relative to the host (0–100 × num_cpus).
	#[serde(rename = "cpuPercentage")]
	pub cpu_percentage: f64,
	/// Memory usage in bytes (usage minus page cache).
	pub memory: f64,
	/// Memory limit configured for the container in bytes (0 = unlimited).
	#[serde(rename = "memoryLimit")]
	pub memory_limit: f64,
	/// Memory usage as a percentage of the limit (0 when limit is 0).
	#[serde(rename = "memoryPercentage")]
	pub memory_percentage: f64,
	/// Cumulative bytes received across all network interfaces since container start.
	#[serde(rename = "networkRxBytes")]
	pub network_rx_bytes: u64,
	/// Cumulative bytes transmitted across all network interfaces since container start.
	#[serde(rename = "networkTxBytes")]
	pub network_tx_bytes: u64,
	/// Cumulative bytes read from block devices since container start.
	#[serde(rename = "blockReadBytes")]
	pub block_read_bytes: u64,
	/// Cumulative bytes written to block devices since container start.
	#[serde(rename = "blockWriteBytes")]
	pub block_write_bytes: u64,
	/// Current number of processes / threads inside the container (`pids_stats.current`).
	pub pids: u64,
}

impl ContainerStatus {
	/// Returns a zeroed [`ContainerStatus`] for a container whose stats could
	/// not be fetched (e.g. a container that stopped between list and stats).
	pub fn empty(id: impl Into<String>) -> Self {
		let id = id.into();
		Self {
			name: String::new(),
			id,
			cpu_percentage: 0.0,
			memory: 0.0,
			memory_limit: 0.0,
			memory_percentage: 0.0,
			network_rx_bytes: 0,
			network_tx_bytes: 0,
			block_read_bytes: 0,
			block_write_bytes: 0,
			pids: 0,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn empty_container_status_zeros() {
		let s = ContainerStatus::empty("abc123");
		assert_eq!(s.id, "abc123");
		assert!(s.name.is_empty());
		assert_eq!(s.cpu_percentage, 0.0);
		assert_eq!(s.network_rx_bytes, 0);
		assert_eq!(s.pids, 0);
	}

	#[test]
	fn container_status_serializes_camel_case_fields() {
		let s = ContainerStatus {
			name: "web".into(),
			id: "id1".into(),
			cpu_percentage: 12.5,
			memory: 100.0,
			memory_limit: 200.0,
			memory_percentage: 50.0,
			network_rx_bytes: 1024,
			network_tx_bytes: 2048,
			block_read_bytes: 4096,
			block_write_bytes: 8192,
			pids: 7,
		};
		let v = serde_json::to_value(&s).unwrap();
		assert_eq!(v["cpuPercentage"], json!(12.5));
		assert_eq!(v["memoryLimit"], json!(200.0));
		assert_eq!(v["memoryPercentage"], json!(50.0));
		assert_eq!(v["networkRxBytes"], json!(1024_u64));
		assert_eq!(v["networkTxBytes"], json!(2048_u64));
		assert_eq!(v["blockReadBytes"], json!(4096_u64));
		assert_eq!(v["blockWriteBytes"], json!(8192_u64));
		assert_eq!(v["pids"], json!(7_u64));
	}

	#[test]
	fn status_serializes_version_fields() {
		let s = Status {
			id: "node1".into(),
			disk: DiskStatus::default(),
			cpu: CpuStatus {
				used_percentage: 0.0,
				cores: 1,
			},
			memory: MemoryStatus {
				total: 0,
				used: 0,
				used_percentage: 0.0,
				free: 0,
			},
			tasks: vec![],
			engine_version: "27.3.1".into(),
			api_version: "1.47".into(),
			kernel_version: "6.1.0".into(),
		};
		let v = serde_json::to_value(&s).unwrap();
		assert_eq!(v["engineVersion"], json!("27.3.1"));
		assert_eq!(v["apiVersion"], json!("1.47"));
		assert_eq!(v["kernelVersion"], json!("6.1.0"));
	}
}
