//! Provider metrics and monitoring

use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Metrics for the provider server
#[derive(Debug)]
pub struct ProviderMetrics {
    requests_total: AtomicU64,
    bytes_served_total: AtomicU64,
    start_time: Instant,
}

impl ProviderMetrics {
    pub fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            bytes_served_total: AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }

    pub fn increment_requests(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_bytes_served(&self, bytes: u64) {
        self.bytes_served_total.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            bytes_served_total: self.bytes_served_total.load(Ordering::Relaxed),
        }
    }
}

impl Default for ProviderMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub requests_total: u64,
    pub bytes_served_total: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    pub status: String,
    pub checks: HealthChecks,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthChecks {
    pub artifacts_readable: bool,
    pub disk_space_ok: bool,
}

impl HealthCheck {
    pub fn is_healthy(&self) -> bool {
        self.checks.artifacts_readable && self.checks.disk_space_ok
    }
}

pub fn check_directory_readable(path: &Path) -> bool {
    path.exists() && std::fs::read_dir(path).is_ok()
}

pub fn perform_health_check(artifacts_dir: &Path) -> HealthCheck {
    let artifacts_readable = check_directory_readable(artifacts_dir);
    let disk_space_ok = artifacts_dir.exists(); // Simplified check

    HealthCheck {
        status: if artifacts_readable && disk_space_ok { "healthy" } else { "unhealthy" }.to_string(),
        checks: HealthChecks {
            artifacts_readable,
            disk_space_ok,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_metrics_increment() {
        let metrics = ProviderMetrics::new();
        metrics.increment_requests();
        metrics.add_bytes_served(1024);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.requests_total, 1);
        assert_eq!(snapshot.bytes_served_total, 1024);
    }

    #[test]
    fn test_health_check() {
        let temp = TempDir::new().unwrap();
        let health = perform_health_check(temp.path());
        assert!(health.is_healthy());
    }
}
