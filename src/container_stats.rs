//! Container stats parsing (Docker CLI–style CPU / memory helpers).

use bollard::container::{CPUStats, MemoryStats, MemoryStatsStats, Stats};
use chrono::{DateTime, Utc};

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

/// Windows CPU % (matches the Docker CLI stats formula).
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

#[inline]
pub fn calculate_memory_usage_unix_no_cache(usage: u64, cache: u64) -> f64 {
    usage.saturating_sub(cache) as f64
}

#[inline]
pub fn calculate_memory_percent_unix_no_cache(limit: f64, used_no_cache: f64) -> f64 {
    if limit != 0.0 {
        used_no_cache / limit * 100.0
    } else {
        0.0
    }
}

fn cache_bytes(mem: &MemoryStats) -> u64 {
    match &mem.stats {
        Some(MemoryStatsStats::V1(v)) => v.cache,
        // cgroup v2: approximate page cache component similarly to Docker heuristics.
        Some(MemoryStatsStats::V2(v)) => v.inactive_file,
        None => 0,
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

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

    crate::models::ContainerStatus {
        name: stats.name.clone(),
        id: stats.id.clone(),
        cpu_percentage: cpu_percent,
        memory,
        memory_limit,
        memory_percentage: memory_percent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_no_cache() {
        let u = calculate_memory_usage_unix_no_cache(500, 200);
        assert!((u - 300.0).abs() < f64::EPSILON);
        let pct = calculate_memory_percent_unix_no_cache(1000.0, 250.0);
        assert!((pct - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn windows_cpu_percent() {
        // 1_000_000 ns raw delta -> poss = 10_000 * 4 = 40_000 with num_procs=4
        let p = calculate_cpu_percent_windows(1_000_000, 4, 25_000);
        assert!((p - 62.5).abs() < 0.01);
    }
}
