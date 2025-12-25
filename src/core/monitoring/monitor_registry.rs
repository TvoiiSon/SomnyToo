use std::sync::Arc;
use std::time::Instant;
use tracing::{info, error};
use tokio::time::{interval, Duration};

use super::unified_monitor::{UnifiedMonitor, SystemHealthReport};
use super::config::MonitoringConfig;
use super::childs::server_monitor::ServerMonitor;

use super::health_checks::{
    database_health::DatabaseHealthCheck,
    network_health::NetworkHealthCheck,
    memory_health::MemoryHealthCheck,
    cpu_health::CpuHealthCheck,
    disk_health::DiskHealthCheck,
    api_health::ApiHealthCheck
};

// Импортируем новые сервисы
use super::health_runner::{HealthCheckRunner, BackgroundMonitorHandle};
use super::metrics_reporter::MetricsReporter;
use super::health_check::{MonitorHealthAdapter, ComponentType};

#[derive(Clone)]
pub struct MonitorRegistry {
    pub unified_monitor: Arc<UnifiedMonitor>,
    pub server_monitor: Arc<ServerMonitor>,
    pub health_runner: HealthCheckRunner,
    pub metrics_reporter: MetricsReporter,
    start_time: Instant,
}

impl MonitorRegistry {
    pub async fn new() -> Self {
        info!("🚀 Initializing comprehensive monitoring system...");

        let config = MonitoringConfig::default();
        let unified_monitor = Arc::new(UnifiedMonitor::new(config));

        // Регистрируем основные мониторы
        info!("📊 Registering core monitors...");

        let server_monitor = ServerMonitor::new();

        unified_monitor.register_monitor(Arc::new(server_monitor.clone())).await;

        // Регистрируем системные health checks
        info!("🔧 Registering system health checks...");

        unified_monitor.register_health_check(Arc::new(DatabaseHealthCheck::new())).await;
        unified_monitor.register_health_check(Arc::new(NetworkHealthCheck::new())).await;
        unified_monitor.register_health_check(Arc::new(MemoryHealthCheck::new())).await;
        unified_monitor.register_health_check(Arc::new(CpuHealthCheck::new())).await;
        unified_monitor.register_health_check(Arc::new(DiskHealthCheck::new())).await;
        unified_monitor.register_health_check(Arc::new(ApiHealthCheck::new())).await;

        // Регистрируем бизнес-мониторы как health checks через адаптер
        unified_monitor.register_health_check(Arc::new(MonitorHealthAdapter::new(
            Arc::new(server_monitor.clone()),
            ComponentType::Service
        ))).await;

        let start_time = Instant::now();
        let health_runner = HealthCheckRunner::new(Arc::clone(&unified_monitor));
        let metrics_reporter = MetricsReporter::new(Arc::clone(&unified_monitor), start_time);

        info!("✅ All monitoring components registered successfully");
        info!("   - Core Monitors: Server"); // Обновляем список
        info!("   - System Checks: Database, Network, Memory, CPU, Disk, API");

        Self {
            unified_monitor,
            server_monitor: Arc::new(server_monitor),
            health_runner,
            metrics_reporter,
            start_time,
        }
    }

    // Делегируем методы новым сервисам
    pub async fn health_check(&self) -> bool {
        self.health_runner.run_health_check().await
    }

    pub async fn get_health_report(&self) -> SystemHealthReport {
        self.health_runner.get_detailed_report().await
    }

    pub async fn print_metrics_report(&self) {
        self.metrics_reporter.print_comprehensive_report().await
    }

    pub async fn get_web_report(&self) -> serde_json::Value {
        self.metrics_reporter.get_web_report().await
    }

    pub fn get_uptime(&self) -> Duration {
        self.metrics_reporter.get_uptime()
    }

    // Запуск фонового мониторинга
    pub async fn start_background_monitoring(self: Arc<Self>) -> BackgroundMonitorHandle {
        let registry = Arc::clone(&self);
        tokio::spawn(async move {
            // Ждем 30 секунд перед первым фоновым check'ом
            tokio::time::sleep(Duration::from_secs(30)).await;

            let mut interval = interval(Duration::from_secs(60)); // Каждую минуту после первой паузы
            let mut error_count = 0;
            const MAX_CONSECUTIVE_ERRORS: u32 = 5;

            loop {
                interval.tick().await;

                // Используем health_runner для безопасной проверки
                let report = registry.health_runner.get_detailed_report().await;

                // Логируем только если есть проблемы
                if !report.overall_health {
                    error_count += 1;
                    error!("🚨 Background health check failed! (consecutive errors: {})", error_count);
                    error!("Critical components: {:?}", report.critical_components);

                    if error_count >= MAX_CONSECUTIVE_ERRORS {
                        error!("Too many consecutive errors ({}), restarting monitoring loop", error_count);
                        error_count = 0;
                        tokio::time::sleep(Duration::from_secs(300)).await; // Пауза 5 минут
                        continue;
                    }
                } else {
                    error_count = 0; // Сбрасываем счетчик при успешной проверке
                }

                // Каждые 5 минут - детальный отчет
                if registry.start_time.elapsed().as_secs() % 300 < 60 {
                    registry.metrics_reporter.print_comprehensive_report().await;
                }
            }
        });

        info!("🔄 Background monitoring started (first check in 30s)");
        BackgroundMonitorHandle::new()
    }
}

impl Default for MonitorRegistry {
    fn default() -> Self {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                Self::new().await
            })
        })
    }
}