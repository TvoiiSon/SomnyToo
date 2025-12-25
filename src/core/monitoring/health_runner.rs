// health_runner.rs - используем правильные макросы

use std::sync::Arc;
use tracing::{info, error, debug};

use super::unified_monitor::{UnifiedMonitor, SystemHealthReport};

#[derive(Clone)]
pub struct HealthCheckRunner {
    unified_monitor: Arc<UnifiedMonitor>,
}

impl HealthCheckRunner {
    pub fn new(unified_monitor: Arc<UnifiedMonitor>) -> Self {
        Self { unified_monitor }
    }

    pub async fn run_health_check(&self) -> bool {
        info!("🔍 Performing comprehensive system health check...");

        let report = self.unified_monitor.comprehensive_health_check().await;

        info!("=== SYSTEM HEALTH REPORT ===");
        info!("Overall Status: {}", if report.overall_health { "✅ HEALTHY" } else { "❌ UNHEALTHY" });

        for (component, status) in &report.components {
            let emoji = if status.is_healthy { "✅" } else { "❌" };
            info!("  {} {}: {}", emoji, component, status.message);

            if let Some(details) = &status.details {
                debug!("    Details: {:?}", details);
            }
        }

        if !report.critical_components.is_empty() {
            error!("🚨 Critical components down: {:?}", report.critical_components);
        }

        if !report.warnings.is_empty() {
            for warning in &report.warnings {
                debug!("⚠️  {}", warning);
            }
        }

        report.overall_health
    }

    pub async fn get_detailed_report(&self) -> SystemHealthReport {
        self.unified_monitor.comprehensive_health_check().await
    }
}

pub struct BackgroundMonitorHandle;

impl BackgroundMonitorHandle {
    pub fn new() -> Self {
        Self
    }

    pub async fn shutdown(self) {
        info!("✅ Background monitoring stopped gracefully");
    }
}