//! Host resource sampling via `sysinfo`.
//!
//! Functions in this module perform targeted, one-shot refreshes so they
//! can be called from the stats tick without keeping a long-lived `System`
//! handle between calls. CPU and memory require a pre-built [`System`]
//! (created with [`new_system`]) because sysinfo needs at least one prior
//! sample to compute a meaningful CPU delta.

use std::path::Path;

use sysinfo::{CpuRefreshKind, Disk, Disks, MemoryRefreshKind, RefreshKind, System};

use crate::models::{CpuStatus, DiskStatus, MemoryStatus};

fn root_disk_candidates() -> &'static [&'static str] {
	if cfg!(windows) {
		&["C:\\", "C:/", "\\"]
	} else {
		&["/"]
	}
}

/// Returns the disk that represents the root filesystem, falling back to the
/// first disk in the list if no canonical mount point is found.
fn pick_root_disk(disks: &Disks) -> Option<&Disk> {
	for c in root_disk_candidates() {
		let p = Path::new(c);
		if let Some(d) = disks.list().iter().find(|d| d.mount_point() == p) {
			return Some(d);
		}
	}
	disks.list().first()
}

/// Samples disk capacity and usage for the root filesystem.
///
/// Returns a zeroed [`DiskStatus`] when no disk is found (e.g. in a
/// stripped container without `/proc/mounts`).
pub fn disk_usage() -> DiskStatus {
	let disks = Disks::new_with_refreshed_list();
	let Some(d) = pick_root_disk(&disks) else {
		return DiskStatus::default();
	};
	let total = d.total_space();
	let free = d.available_space();
	let used = total.saturating_sub(free);
	let used_percentage = if total > 0 {
		(used as f64 / total as f64) * 100.0
	} else {
		0.0
	};
	DiskStatus {
		total,
		used,
		used_percentage,
		free,
	}
}

/// Refreshes CPU usage and returns the current global percentage.
///
/// The `cores_reported` value comes from `docker info` rather than sysinfo
/// so it reflects the number of CPUs visible to the Docker daemon, which
/// may differ from the host topology (e.g. in VMs or cgroup-limited nodes).
pub fn cpu_usage(sys: &mut System, cores_reported: i32) -> CpuStatus {
	sys.refresh_cpu_usage();
	CpuStatus {
		used_percentage: sys.global_cpu_usage() as f64,
		cores: cores_reported,
	}
}

/// Refreshes memory statistics and returns total, used, free, and percentage.
pub fn memory_usage(sys: &mut System) -> MemoryStatus {
	sys.refresh_memory();
	let total = sys.total_memory();
	let used = sys.used_memory();
	let free = sys.free_memory();
	let used_percentage = if total > 0 {
		(used as f64 / total as f64) * 100.0
	} else {
		0.0
	};
	MemoryStatus {
		total,
		used,
		used_percentage,
		free,
	}
}

/// Creates a [`System`] pre-configured to refresh only CPU and memory data.
pub fn new_system() -> System {
	System::new_with_specifics(
		RefreshKind::new()
			.with_cpu(CpuRefreshKind::everything())
			.with_memory(MemoryRefreshKind::everything()),
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn disk_usage_percent_in_range() {
		let d = disk_usage();
		assert!(d.used_percentage >= 0.0 && d.used_percentage <= 100.0);
		if d.total > 0 {
			assert!(d.used <= d.total);
			assert!(d.free <= d.total);
		}
	}

	#[test]
	fn memory_and_cpu_sample() {
		let mut sys = new_system();
		let mem = memory_usage(&mut sys);
		assert!(mem.used_percentage >= 0.0 && mem.used_percentage <= 100.0);
		let cpu = cpu_usage(&mut sys, 1);
		assert!(cpu.cores >= 1);
		assert!(cpu.used_percentage >= 0.0);
	}
}
