use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct HealthStatus {
    pub component: String,
    pub status: HealthState,
    pub message: String,
    pub last_check: u64, // Используем timestamp вместо Instant
    pub response_time_ms: u64, // В миллисекундах
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unhealthy,
}

pub struct HealthChecker {
    components: RwLock<HashMap<String, HealthStatus>>,
    check_interval: Duration,
}

impl HealthChecker {
    pub fn new(check_interval: Duration) -> Self {
        Self {
            components: RwLock::new(HashMap::new()),
            check_interval,
        }
    }

    pub async fn register_component(&self, name: String) {
        let mut components = self.components.write().await;
        components.insert(name.clone(), HealthStatus {
            component: name,
            status: HealthState::Healthy,
            message: "Initialized".to_string(),
            last_check: Self::current_timestamp(),
            response_time_ms: 0,
        });
    }

    pub async fn start_periodic_checks(self: Arc<Self>) {
        let check_interval = self.check_interval;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(check_interval);

            loop {
                interval.tick().await;
                self.run_health_checks().await;
            }
        });
    }

    async fn run_health_checks(&self) {
        let components: Vec<String> = {
            let components = self.components.read().await;
            components.keys().cloned().collect()
        };

        for component in components {
            // Здесь можно добавить реальные проверки здоровья для каждого компонента
            let health_state = HealthState::Healthy;
            let message = "Periodic check passed".to_string();
            let response_time = Duration::from_millis(1);

            self.update_status(&component, health_state, message, response_time).await;
        }
    }

    // Добавляем метод для принудительной проверки
    pub async fn force_check(&self, component: &str) -> HealthStatus {
        let check_start = std::time::Instant::now();

        // Здесь можно добавить специфичную логику проверки для компонента
        let health_state = HealthState::Healthy;
        let message = "Forced check completed".to_string();
        let response_time = check_start.elapsed();

        self.update_status(component, health_state, message, response_time).await;

        let components = self.components.read().await;
        components.get(component).cloned().unwrap_or_else(|| HealthStatus {
            component: component.to_string(),
            status: HealthState::Unhealthy,
            message: "Component not found".to_string(),
            last_check: Self::current_timestamp(),
            response_time_ms: response_time.as_millis() as u64,
        })
    }

    pub async fn update_status(&self, component: &str, status: HealthState, message: String, response_time: Duration) {
        let mut components = self.components.write().await;
        if let Some(health_status) = components.get_mut(component) {
            health_status.status = status;
            health_status.message = message;
            health_status.last_check = Self::current_timestamp();
            health_status.response_time_ms = response_time.as_millis() as u64;
        }
    }

    pub async fn get_overall_health(&self) -> HealthState {
        let components = self.components.read().await;

        if components.is_empty() {
            return HealthState::Unhealthy;
        }

        let mut unhealthy_count = 0;
        let mut degraded_count = 0;

        for status in components.values() {
            match status.status {
                HealthState::Unhealthy => unhealthy_count += 1,
                HealthState::Degraded => degraded_count += 1,
                HealthState::Healthy => {}
            }
        }

        if unhealthy_count > 0 {
            HealthState::Unhealthy
        } else if degraded_count > 0 {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        }
    }

    pub async fn get_detailed_status(&self) -> Vec<HealthStatus> {
        let components = self.components.read().await;
        components.values().cloned().collect()
    }

    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}