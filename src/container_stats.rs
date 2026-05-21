//! Container stats parsing — CPU, memory, network I/O, block I/O, PIDs.
//!
//! All formulas match the Docker CLI `docker stats` implementation so that
//! the values displayed in Swarmboty are consistent with what operators see
//! on the command line.

use bollard::container::{CPUStats, MemoryStats, MemoryStatsStats, Stats};
use chrono::{DateTime, Utc};

// ── CPU ───────────────────────────────────────────────────────────────────────

/// Calculates the CPU usage percentage for Linux/macOS containers.
///
/// Uses the same formula as the Docker CLI:
/// `(cpu_delta / system_delta) * online_cpus * 100`.
///
/// Returns `0.0` when there is no measurable delta (e.g. the first sample
/// after container start, or a container that has done no work).
#[inline]
pub fn calculate_cpu_percent_unix(previous_cpu: u64, previous_system: u64, cur: &CPUStats) -> f64 {
	let cpu_delta = cur.cpu_usage.total_usage.saturating_sub(previous_cpu) as f64;
	let system_delta = cur
		.system_cpu_usage
		.unwrap_or(0)
		.saturating_sub(previous_system) as f64;
	let mut online_cpus = cur.online_cpus.unwrap_or(0) as f64;
	if online_cpus == 0.0 {
		online_cpus = cur
			.cpu_usage
			.percpu_usage
			.as_ref()
			.map(|v| v.len())
			.unwrap_or(0) as f64;
	}
	if system_delta > 0.0 && cpu_delta > 0.0 {
		(cpu_delta / system_delta) * online_cpus * 100.0
	} else {
		0.0
	}
}

/// Calculates the CPU usage percentage for Windows containers.
///
/// Uses the Docker CLI formula:
/// `intervals_used / (read_preread_ns / 100 * num_procs) * 100`.
#[inline]
pub fn calculate_cpu_percent_windows(
	read_preread_ns: u64,
	num_procs: u32,
	intervals_used: u64,
) -> f64 {
	let poss = read_preread_ns
		.saturating_div(100)
		.saturating_mul(num_procs as u64);
	if poss > 0 {
		intervals_used as f64 / poss as f64 * 100.0
	} else {
		0.0
	}
}

// ── Memory ────────────────────────────────────────────────────────────────────

/// Returns `usage − cache` bytes, clamped to zero on underflow.
///
/// Subtracting page-cache matches the Docker CLI "mem usage" column and
/// gives the true resident set size of the container workload.
#[inline]
pub fn calculate_memory_usage_unix_no_cache(usage: u64, cache: u64) -> f64 {
	usage.saturating_sub(cache) as f64
}

/// Returns `used_no_cache / limit * 100`, or `0.0` when `limit` is zero.
#[inline]
pub fn calculate_memory_percent_unix_no_cache(limit: f64, used_no_cache: f64) -> f64 {
	if limit != 0.0 {
		used_no_cache / limit * 100.0
	} else {
		0.0
	}
}

/// Extracts the page-cache size from a [`MemoryStats`] snapshot.
///
/// - **cgroup v1**: uses the `cache` field.
/// - **cgroup v2**: uses `inactive_file`, which is the closest equivalent
///   and the value the Docker CLI uses.
fn cache_bytes(mem: &MemoryStats) -> u64 {
	match &mem.stats {
		Some(MemoryStatsStats::V1(v)) => v.cache,
		Some(MemoryStatsStats::V2(v)) => v.inactive_file,
		None => 0,
	}
}

// ── Network I/O ───────────────────────────────────────────────────────────────

/// Sums `rx_bytes` and `tx_bytes` across all network interfaces in the stats
/// snapshot.  Returns `(rx_bytes, tx_bytes)`.
///
/// Available from Docker API v1.21+ for Linux containers; the `networks` map
/// will be empty on Windows (network counters are not exposed via this endpoint).
pub fn network_io(stats: &Stats) -> (u64, u64) {
	let mut rx = 0u64;
	let mut tx = 0u64;
	if let Some(nets) = &stats.networks {
		for iface in nets.values() {
			rx = rx.saturating_add(iface.rx_bytes);
			tx = tx.saturating_add(iface.tx_bytes);
		}
	}
	(rx, tx)
}

// ── Block I/O ─────────────────────────────────────────────────────────────────

/// Sums block device read and write bytes from `blkio_stats`.
/// Returns `(read_bytes, write_bytes)`.
///
/// Reads `blkio_stats.io_service_bytes_recursive`, which is populated for
/// both cgroup v1 and v2 on Linux.  On Windows, `blkio_stats` is not
/// populated and both values will be zero.
pub fn block_io(stats: &Stats) -> (u64, u64) {
	let mut read = 0u64;
	let mut write = 0u64;
	if let Some(entries) = stats.blkio_stats.io_service_bytes_recursive.as_deref() {
		for entry in entries {
			match entry.op.to_ascii_lowercase().as_str() {
				"read" => read = read.saturating_add(entry.value),
				"write" => write = write.saturating_add(entry.value),
				_ => {}
			}
		}
	}
	(read, write)
}

// ── PIDs ──────────────────────────────────────────────────────────────────────

/// Returns the current PID count inside the container (`pids_stats.current`).
///
/// Returns `0` when the field is absent, which happens on Windows or on
/// kernels compiled without the `pids` cgroup controller.
#[inline]
pub fn pid_count(stats: &Stats) -> u64 {
	stats.pids_stats.current.unwrap_or(0)
}

// ── Composite builder ─────────────────────────────────────────────────────────

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
	DateTime::parse_from_rfc3339(s.trim())
		.ok()
		.map(|d| d.with_timezone(&Utc))
}

/// Converts a raw [`Stats`] snapshot into a [`ContainerStatus`] model.
///
/// `daemon_os_type` is the `Os` field from `docker info` (case-insensitive).
/// When it equals `"windows"`, Windows-specific CPU and memory formulas are
/// used; otherwise the Linux/cgroup path is taken.
pub fn container_status_from_stats(
	stats: &Stats,
	daemon_os_type: &str,
) -> crate::models::ContainerStatus {
	let is_windows = daemon_os_type.eq_ignore_ascii_case("windows");

	let (cpu_percent, memory, memory_limit, memory_percent) = if is_windows {
		let interval_ns = match (parse_ts(&stats.read), parse_ts(&stats.preread)) {
			(Some(r), Some(p)) => r
				.signed_duration_since(p)
				.num_nanoseconds()
				.unwrap_or(0)
				.max(0) as u64,
			_ => 0,
		};
		let used = stats
			.cpu_stats
			.cpu_usage
			.total_usage
			.saturating_sub(stats.precpu_stats.cpu_usage.total_usage);
		let cpu = calculate_cpu_percent_windows(interval_ns, stats.num_procs, used);
		let mem = stats.memory_stats.privateworkingset.unwrap_or(0) as f64;
		(cpu, mem, 0.0, 0.0)
	} else {
		let prev_cpu = stats.precpu_stats.cpu_usage.total_usage;
		let prev_sys = stats.precpu_stats.system_cpu_usage.unwrap_or(0);
		let cpu = calculate_cpu_percent_unix(prev_cpu, prev_sys, &stats.cpu_stats);
		let cache = cache_bytes(&stats.memory_stats);
		let usage = stats.memory_stats.usage.unwrap_or(0);
		let memory = calculate_memory_usage_unix_no_cache(usage, cache);
		let memory_limit = stats.memory_stats.limit.unwrap_or(0) as f64;
		let memory_percent = calculate_memory_percent_unix_no_cache(memory_limit, memory);
		(cpu, memory, memory_limit, memory_percent)
	};

	let (network_rx_bytes, network_tx_bytes) = network_io(stats);
	let (block_read_bytes, block_write_bytes) = block_io(stats);

	crate::models::ContainerStatus {
		name: stats.name.clone(),
		id: stats.id.clone(),
		cpu_percentage: cpu_percent,
		memory,
		memory_limit,
		memory_percentage: memory_percent,
		network_rx_bytes,
		network_tx_bytes,
		block_read_bytes,
		block_write_bytes,
		pids: pid_count(stats),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bollard::container::{BlkioStats, BlkioStatsEntry, NetworkStats, PidsStats};
	use std::collections::HashMap;

	#[test]
	fn memory_no_cache() {
		let u = calculate_memory_usage_unix_no_cache(500, 200);
		assert!((u - 300.0).abs() < f64::EPSILON);
		let pct = calculate_memory_percent_unix_no_cache(1000.0, 250.0);
		assert!((pct - 25.0).abs() < f64::EPSILON);
	}

	#[test]
	fn memory_percent_zero_when_limit_zero() {
		assert_eq!(calculate_memory_percent_unix_no_cache(0.0, 100.0), 0.0);
	}

	#[test]
	fn windows_cpu_percent() {
		let p = calculate_cpu_percent_windows(1_000_000, 4, 25_000);
		assert!((p - 62.5).abs() < 0.01);
	}

	fn empty_throttling() -> bollard::container::ThrottlingData {
		bollard::container::ThrottlingData {
			periods: 0,
			throttled_periods: 0,
			throttled_time: 0,
		}
	}

	#[test]
	fn unix_cpu_percent_with_online_cpus() {
		use bollard::container::{CPUStats, CPUUsage};

		let cur = CPUStats {
			cpu_usage: CPUUsage {
				total_usage: 200,
				percpu_usage: None,
				usage_in_usermode: 0,
				usage_in_kernelmode: 0,
			},
			system_cpu_usage: Some(2000),
			online_cpus: Some(2),
			throttling_data: empty_throttling(),
		};
		let p = calculate_cpu_percent_unix(100, 1000, &cur);
		assert!((p - 20.0).abs() < f64::EPSILON);
	}

	#[test]
	fn unix_cpu_percent_zero_when_no_delta() {
		use bollard::container::{CPUStats, CPUUsage};

		let cur = CPUStats {
			cpu_usage: CPUUsage {
				total_usage: 0,
				percpu_usage: None,
				usage_in_usermode: 0,
				usage_in_kernelmode: 0,
			},
			system_cpu_usage: Some(1000),
			online_cpus: Some(1),
			throttling_data: empty_throttling(),
		};
		assert_eq!(calculate_cpu_percent_unix(0, 1000, &cur), 0.0);
	}

	fn make_network_stats(rx: u64, tx: u64) -> NetworkStats {
		NetworkStats {
			rx_bytes: rx,
			rx_packets: 0,
			rx_errors: 0,
			rx_dropped: 0,
			tx_bytes: tx,
			tx_packets: 0,
			tx_errors: 0,
			tx_dropped: 0,
		}
	}

	#[test]
	fn network_io_sums_interfaces() {
		use bollard::container::{BlkioStats, CPUStats, CPUUsage, MemoryStats, PidsStats, Stats};

		let mut nets = HashMap::new();
		nets.insert("eth0".to_string(), make_network_stats(1000, 2000));
		nets.insert("eth1".to_string(), make_network_stats(500, 300));

		let stats = Stats {
			name: "c".to_string(),
			id: "id".to_string(),
			read: "2024-01-01T00:00:00Z".to_string(),
			preread: "2024-01-01T00:00:00Z".to_string(),
			num_procs: 0,
			networks: Some(nets),
			blkio_stats: BlkioStats {
				io_service_bytes_recursive: None,
				io_serviced_recursive: None,
				io_queue_recursive: None,
				io_service_time_recursive: None,
				io_wait_time_recursive: None,
				io_merged_recursive: None,
				io_time_recursive: None,
				sectors_recursive: None,
			},
			pids_stats: PidsStats {
				current: Some(3),
				limit: None,
			},
			cpu_stats: CPUStats {
				cpu_usage: CPUUsage {
					total_usage: 0,
					percpu_usage: None,
					usage_in_usermode: 0,
					usage_in_kernelmode: 0,
				},
				system_cpu_usage: None,
				online_cpus: None,
				throttling_data: empty_throttling(),
			},
			precpu_stats: CPUStats {
				cpu_usage: CPUUsage {
					total_usage: 0,
					percpu_usage: None,
					usage_in_usermode: 0,
					usage_in_kernelmode: 0,
				},
				system_cpu_usage: None,
				online_cpus: None,
				throttling_data: empty_throttling(),
			},
			memory_stats: MemoryStats {
				stats: None,
				max_usage: None,
				usage: None,
				failcnt: None,
				limit: None,
				privateworkingset: None,
			},
			storage_stats: None,
		};

		let (rx, tx) = network_io(&stats);
		assert_eq!(rx, 1500);
		assert_eq!(tx, 2300);
		assert_eq!(pid_count(&stats), 3);
	}

	#[test]
	fn block_io_sums_read_and_write() {
		use bollard::container::{BlkioStats, CPUStats, CPUUsage, MemoryStats, PidsStats, Stats};

		let entries = vec![
			BlkioStatsEntry {
				major: 8,
				minor: 0,
				op: "Read".to_string(),
				value: 4096,
			},
			BlkioStatsEntry {
				major: 8,
				minor: 0,
				op: "Write".to_string(),
				value: 8192,
			},
			BlkioStatsEntry {
				major: 8,
				minor: 0,
				op: "Total".to_string(),
				value: 12288,
			},
		];

		let stats = Stats {
			name: "c".to_string(),
			id: "id".to_string(),
			read: "2024-01-01T00:00:00Z".to_string(),
			preread: "2024-01-01T00:00:00Z".to_string(),
			num_procs: 0,
			networks: None,
			blkio_stats: BlkioStats {
				io_service_bytes_recursive: Some(entries),
				io_serviced_recursive: None,
				io_queue_recursive: None,
				io_service_time_recursive: None,
				io_wait_time_recursive: None,
				io_merged_recursive: None,
				io_time_recursive: None,
				sectors_recursive: None,
			},
			pids_stats: PidsStats {
				current: None,
				limit: None,
			},
			cpu_stats: CPUStats {
				cpu_usage: CPUUsage {
					total_usage: 0,
					percpu_usage: None,
					usage_in_usermode: 0,
					usage_in_kernelmode: 0,
				},
				system_cpu_usage: None,
				online_cpus: None,
				throttling_data: empty_throttling(),
			},
			precpu_stats: CPUStats {
				cpu_usage: CPUUsage {
					total_usage: 0,
					percpu_usage: None,
					usage_in_usermode: 0,
					usage_in_kernelmode: 0,
				},
				system_cpu_usage: None,
				online_cpus: None,
				throttling_data: empty_throttling(),
			},
			memory_stats: MemoryStats {
				stats: None,
				max_usage: None,
				usage: None,
				failcnt: None,
				limit: None,
				privateworkingset: None,
			},
			storage_stats: None,
		};

		let (read, write) = block_io(&stats);
		assert_eq!(read, 4096);
		assert_eq!(write, 8192);
	}
}
