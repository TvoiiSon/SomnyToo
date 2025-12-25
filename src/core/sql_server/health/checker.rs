use std::time::{Instant, Duration};
use tokio::sync::RwLock;
use std::sync::Arc;

pub struct HealthChecker {
    check_interval: Duration,
    last_check: Arc<RwLock<Option<Instant>>>,
    health_status: Arc<RwLock<HealthStatus>>,
}

#[derive(Debug, Clone)]
pub struct HealthStatus {
    pub is_healthy: bool,
    pub last_check: Instant,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    pub component_status: Vec<ComponentStatus>, // ✅ ДОБАВЛЕНО: статус компонентов
}

#[derive(Debug, Clone)]
pub struct ComponentStatus {
    pub name: String,
    pub is_healthy: bool,
    pub response_time: Duration,
    pub last_error: Option<String>,
}

impl HealthStatus {
    pub fn new() -> Self {
        Self {
            is_healthy: false,
            last_check: Instant::now(),
            last_error: None,
            consecutive_failures: 0,
            component_status: Vec::new(),
        }
    }
}

impl HealthChecker {
    pub fn new(check_interval: Duration) -> Self {
        Self {
            check_interval,
            last_check: Arc::new(RwLock::new(None)),
            health_status: Arc::new(RwLock::new(HealthStatus::new())),
        }
    }

    // ✅ ДОБАВЛЕНО: Запуск периодических проверок
    pub async fn start_periodic_checks(self: Arc<Self>) {
        let checker = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(checker.check_interval);
            loop {
                interval.tick().await;
                checker.perform_health_check().await;
            }
        });
    }

    pub async fn perform_health_check(&self) -> HealthStatus {
        let mut status = HealthStatus::new();
        status.last_check = Instant::now();

        // ✅ ДОБАВЛЕНО: Проверка различных компонентов системы
        let components = self.check_components().await;
        status.component_status = components;

        // Определяем общий статус здоровья
        status.is_healthy = status.component_status.iter()
            .all(|component| component.is_healthy);

        if !status.is_healthy {
            status.consecutive_failures += 1;
            status.last_error = Some("One or more components are unhealthy".to_string());
        } else {
            status.consecutive_failures = 0;
            status.last_error = None;
        }

        // Сохраняем статус
        {
            let mut health_status = self.health_status.write().await;
            *health_status = status.clone();
        }

        // Обновляем время последней проверки
        {
            let mut last_check = self.last_check.write().await;
            *last_check = Some(Instant::now());
        }

        status
    }

    async fn check_components(&self) -> Vec<ComponentStatus> {
        let mut components = Vec::new();

        // ✅ ДОБАВЛЕНО: Проверка различных компонентов системы
        // В реальной реализации здесь будут проверки подключения к БД,
        // кэша, реплик и других компонентов

        // Заглушки для демонстрации
        components.push(ComponentStatus {
            name: "database".to_string(),
            is_healthy: true,
            response_time: Duration::from_millis(10),
            last_error: None,
        });

        components.push(ComponentStatus {
            name: "cache".to_string(),
            is_healthy: true,
            response_time: Duration::from_millis(5),
            last_error: None,
        });

        components.push(ComponentStatus {
            name: "replicas".to_string(),
            is_healthy: true,
            response_time: Duration::from_millis(15),
            last_error: None,
        });

        components
    }

    // ✅ ДОБАВЛЕНО: Получение текущего статуса
    pub async fn get_current_status(&self) -> HealthStatus {
        let health_status = self.health_status.read().await;
        health_status.clone()
    }

    // ✅ ДОБАВЛЕНО: Проверка, нужно ли выполнять проверку
    pub async fn should_check(&self) -> bool {
        let last_check = self.last_check.read().await;
        match *last_check {
            Some(time) => Instant::now().duration_since(time) >= self.check_interval,
            None => true,
        }
    }

    // ✅ ДОБАВЛЕНО: Принудительная проверка
    pub async fn force_check(&self) -> HealthStatus {
        self.perform_health_check().await
    }
}