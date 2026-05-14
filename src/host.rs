//! Host resource sampling via sysinfo (targeted refresh).

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

fn pick_root_disk(disks: &Disks) -> Option<&Disk> {
    for c in root_disk_candidates() {
        let p = Path::new(c);
        if let Some(d) = disks.list().iter().find(|d| d.mount_point() == p) {
            return Some(d);
        }
    }
    disks.list().first()
}

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

/// CPU usage from sysinfo (`global_cpu_usage` after one refresh; first samples may be low).
pub fn cpu_usage(sys: &mut System, cores_reported: i32) -> CpuStatus {
    sys.refresh_cpu_usage();
    CpuStatus {
        used_percentage: sys.global_cpu_usage() as f64,
        cores: cores_reported,
    }
}

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

pub fn new_system() -> System {
    System::new_with_specifics(
        RefreshKind::new()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything()),
    )
}
