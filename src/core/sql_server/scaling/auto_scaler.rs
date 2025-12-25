use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoScalingConfig {
    pub enable_auto_scaling: bool,
    pub max_replicas: usize,
    pub scale_up_cpu_threshold: f64,
    pub scale_down_cpu_threshold: f64,
    pub scale_up_connections_threshold: usize,
    pub check_interval_seconds: u64,
}

impl Default for AutoScalingConfig {
    fn default() -> Self {
        Self {
            enable_auto_scaling: true,
            max_replicas: 10,
            scale_up_cpu_threshold: 80.0,
            scale_down_cpu_threshold: 30.0,
            scale_up_connections_threshold: 1000,
            check_interval_seconds: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SystemMetrics {
    pub cpu_usage: f64,
    pub memory_usage: f64,
    pub active_connections: usize,
    pub queries_per_second: f64,
}

pub struct AutoScaler {
    config: AutoScalingConfig,
    current_replicas: Arc<RwLock<usize>>,
    metrics_history: Arc<RwLock<Vec<SystemMetrics>>>,
}

impl AutoScaler {
    pub fn new(config: AutoScalingConfig) -> Self {
        Self {
            config,
            current_replicas: Arc::new(RwLock::new(1)),
            metrics_history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    // ✅ ДОБАВЛЕНО: Основной метод автоскейлинга
    pub async fn check_and_scale(&self, current_metrics: SystemMetrics) -> ScalingDecision {
        if !self.config.enable_auto_scaling {
            return ScalingDecision::NoAction;
        }

        // Сохраняем метрики для анализа тренда
        {
            let mut history = self.metrics_history.write().await;
            history.push(current_metrics.clone());
            // Сохраняем только последние 10 метрик
            if history.len() > 10 {
                history.remove(0);
            }
        }

        let current_replicas = *self.current_replicas.read().await;

        // Проверяем условия для масштабирования вверх
        if self.should_scale_up(&current_metrics, current_replicas).await {
            let new_replicas = (current_replicas + 1).min(self.config.max_replicas);
            *self.current_replicas.write().await = new_replicas;
            return ScalingDecision::ScaleUp(new_replicas);
        }

        // Проверяем условия для масштабирования вниз
        if self.should_scale_down(&current_metrics, current_replicas).await {
            let new_replicas = current_replicas.saturating_sub(1).max(1);
            *self.current_replicas.write().await = new_replicas;
            return ScalingDecision::ScaleDown(new_replicas);
        }

        ScalingDecision::NoAction
    }

    async fn should_scale_up(&self, metrics: &SystemMetrics, current_replicas: usize) -> bool {
        if current_replicas >= self.config.max_replicas {
            return false;
        }

        // Условия для масштабирования вверх
        metrics.cpu_usage > self.config.scale_up_cpu_threshold ||
            metrics.active_connections > self.config.scale_up_connections_threshold ||
            metrics.queries_per_second > 1000.0 // Дополнительное условие
    }

    async fn should_scale_down(&self, metrics: &SystemMetrics, current_replicas: usize) -> bool {
        if current_replicas <= 1 {
            return false;
        }

        // Условия для масштабирования вниз
        metrics.cpu_usage < self.config.scale_down_cpu_threshold &&
            metrics.active_connections < (self.config.scale_up_connections_threshold / 2) &&
            metrics.queries_per_second < 100.0
    }

    // ✅ ДОБАВЛЕНО: Метод для получения текущего состояния
    pub async fn get_status(&self) -> AutoScalerStatus {
        let current_replicas = *self.current_replicas.read().await;
        let history = self.metrics_history.read().await;

        AutoScalerStatus {
            enabled: self.config.enable_auto_scaling,
            current_replicas,
            max_replicas: self.config.max_replicas,
            metrics_history_count: history.len(),
        }
    }

    // ✅ ДОБАВЛЕНО: Метод для ручного управления репликами
    pub async fn set_replica_count(&self, count: usize) -> Result<(), String> {
        if count > self.config.max_replicas {
            return Err(format!("Replica count cannot exceed {}", self.config.max_replicas));
        }

        *self.current_replicas.write().await = count;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum ScalingDecision {
    NoAction,
    ScaleUp(usize),
    ScaleDown(usize),
}

pub struct AutoScalerStatus {
    pub enabled: bool,
    pub current_replicas: usize,
    pub max_replicas: usize,
    pub metrics_history_count: usize,
}