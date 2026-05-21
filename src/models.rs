//! JSON payloads sent to Swarmbot.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Status {
    pub id: String,
    pub disk: DiskStatus,
    pub cpu: CpuStatus,
    pub memory: MemoryStatus,
    pub tasks: Vec<ContainerStatus>,
}

#[derive(Debug, Default, Serialize)]
pub struct DiskStatus {
    pub total: u64,
    pub used: u64,
    pub used_percentage: f64,
    pub free: u64,
}

#[derive(Debug, Serialize)]
pub struct CpuStatus {
    pub used_percentage: f64,
    pub cores: i32,
}

#[derive(Debug, Serialize)]
pub struct MemoryStatus {
    pub total: u64,
    pub used: u64,
    pub used_percentage: f64,
    pub free: u64,
}

#[derive(Debug, Serialize)]
pub struct ContainerStatus {
    pub name: String,
    pub id: String,
    #[serde(rename = "cpuPercentage")]
    pub cpu_percentage: f64,
    pub memory: f64,
    #[serde(rename = "memoryLimit")]
    pub memory_limit: f64,
    #[serde(rename = "memoryPercentage")]
    pub memory_percentage: f64,
}

impl ContainerStatus {
    pub fn empty(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: String::new(),
            id,
            cpu_percentage: 0.0,
            memory: 0.0,
            memory_limit: 0.0,
            memory_percentage: 0.0,
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
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["cpuPercentage"], json!(12.5));
        assert_eq!(v["memoryLimit"], json!(200.0));
        assert_eq!(v["memoryPercentage"], json!(50.0));
    }
}
