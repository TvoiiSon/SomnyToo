use std::sync::Arc;
use tracing::info;

use super::unified_monitor::UnifiedMonitor;

#[derive(Clone)]
pub struct MetricsReporter {
    unified_monitor: Arc<UnifiedMonitor>,
    start_time: std::time::Instant,
}

impl MetricsReporter {
    pub fn new(unified_monitor: Arc<UnifiedMonitor>, start_time: std::time::Instant) -> Self {
        Self { unified_monitor, start_time }
    }

    pub async fn print_comprehensive_report(&self) {
        info!("📈 Generating comprehensive metrics report...");

        let all_metrics = self.unified_monitor.collect_all_metrics().await;
        let health_report = self.unified_monitor.comprehensive_health_check().await;

        info!("=== COMPREHENSIVE METRICS REPORT ===");
        info!("System Health: {}", if health_report.overall_health { "✅" } else { "❌" });

        for (monitor_name, metrics) in all_metrics {
            info!("--- {} Metrics ---", monitor_name);
            for (metric_name, metric_value) in metrics.metrics {
                info!("  {}: {:?}", metric_name, metric_value);
            }
            info!("  Health: {}", metrics.health);
        }

        // Показываем системные ресурсы
        if let Ok(memory_info) = sys_info::mem_info() {
            let memory_usage = ((memory_info.total - memory_info.avail) as f64 / memory_info.total as f64) * 100.0;
            info!("--- System Resources ---");
            info!("  Memory: {:.1}% used ({}/{} MB)",
                  memory_usage,
                  (memory_info.total - memory_info.avail) / 1024,
                  memory_info.total / 1024);
        }

        if let Ok(load_avg) = sys_info::loadavg() {
            info!("  CPU Load: {:.2}, {:.2}, {:.2}",
                  load_avg.one, load_avg.five, load_avg.fifteen);
        }
    }

    pub async fn get_web_report(&self) -> serde_json::Value {
        self.unified_monitor.generate_web_report().await
    }

    pub fn get_uptime(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }
}