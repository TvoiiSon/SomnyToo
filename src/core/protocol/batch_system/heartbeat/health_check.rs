use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::time::interval;
use tracing::{info, warn, debug, error};

use super::batch_system_monitor::{BatchSystemMonitor, ComponentHealth};

pub struct BatchSystemHealthChecker {
    monitor: Arc<BatchSystemMonitor>,
    check_interval: Duration,
    last_check_time: Arc<std::sync::Mutex<SystemTime>>,
    consecutive_failures: Arc<std::sync::Mutex<u32>>,
    max_consecutive_failures: u32,
}

impl BatchSystemHealthChecker {
    pub fn new(monitor: Arc<BatchSystemMonitor>, check_interval: Duration) -> Self {
        Self {
            monitor,
            check_interval,
            last_check_time: Arc::new(std::sync::Mutex::new(SystemTime::now())),
            consecutive_failures: Arc::new(std::sync::Mutex::new(0)),
            max_consecutive_failures: 3,
        }
    }

    pub async fn start(self: Arc<Self>) {
        let mut interval_timer = interval(self.check_interval);

        loop {
            interval_timer.tick().await;
            self.run_health_check().await;
        }
    }

    async fn run_health_check(&self) {
        debug!("Running batch system health check...");

        let start_time = SystemTime::now();

        // Собираем метрики
        let _metrics = self.monitor.collect_metrics().await;

        // Проверяем здоровье компонентов
        let health_statuses = self.monitor.check_components_health().await;

        // Отправляем алерты
        self.monitor.send_alerts().await;

        let check_duration = start_time.elapsed().unwrap_or(Duration::from_secs(0));

        // Логируем результаты
        self.log_health_status(&health_statuses, check_duration).await;

        // Обновляем счетчик последовательных сбоев
        let overall_healthy = !health_statuses.values().any(|h|
            *h == ComponentHealth::Unhealthy || *h == ComponentHealth::Critical
        );

        let mut failures = self.consecutive_failures.lock().unwrap();
        if overall_healthy {
            *failures = 0;
        } else {
            *failures += 1;

            if *failures >= self.max_consecutive_failures {
                error!("Batch system has had {} consecutive health check failures!",
                    *failures);
                // Здесь можно добавить действия при повторных сбоях
                // Например, перезапуск компонентов или отправку критических алертов
            }
        }
        drop(failures); // Освобождаем блокировку

        // Обновляем время последней проверки
        let mut last_check = self.last_check_time.lock().unwrap();
        *last_check = SystemTime::now();
    }

    async fn log_health_status(&self, health_statuses: &std::collections::HashMap<String, ComponentHealth>,
                               duration: Duration) {
        let mut healthy_count = 0;
        let mut degraded_count = 0;
        let mut unhealthy_count = 0;
        let mut critical_count = 0;

        for (component, health) in health_statuses {
            match health {
                ComponentHealth::Healthy => {
                    debug!("✅ Component {}: Healthy", component);
                    healthy_count += 1;
                }
                ComponentHealth::Degraded => {
                    warn!("⚠️ Component {}: Degraded", component);
                    degraded_count += 1;
                }
                ComponentHealth::Unhealthy => {
                    error!("❌ Component {}: Unhealthy", component);
                    unhealthy_count += 1;
                }
                ComponentHealth::Critical => {
                    error!("🚨 Component {}: Critical", component);
                    critical_count += 1;
                }
            }
        }

        let total = health_statuses.len();
        info!("Batch system health check completed in {:?}: {}/{} healthy, {} degraded, {} unhealthy, {} critical",
            duration, healthy_count, total, degraded_count, unhealthy_count, critical_count);
    }

    pub async fn emergency_check(&self) -> bool {
        debug!("Running emergency batch system health check...");

        let health_statuses = self.monitor.check_components_health().await;

        // Для экстренной проверки сразу отправляем алерты
        self.monitor.send_alerts().await;

        // Возвращаем true, если нет критических проблем
        !health_statuses.values().any(|h| *h == ComponentHealth::Critical)
    }

    pub fn get_consecutive_failures(&self) -> u32 {
        *self.consecutive_failures.lock().unwrap()
    }

    pub fn get_last_check_time(&self) -> SystemTime {
        *self.last_check_time.lock().unwrap()
    }
}