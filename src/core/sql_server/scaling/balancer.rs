use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;
use super::super::security::node::DatabaseNode;
use std::time::{Instant, Duration};

#[derive(Debug, Clone)]
pub enum LoadBalancingStrategy {
    RoundRobin,
    WeightedRoundRobin,
    LeastConnections,
    LatencyBased, // ✅ ДОБАВЛЕНО: на основе задержки
}

pub struct ReadReplicaBalancer {
    replicas: Arc<RwLock<Vec<DatabaseNode>>>,
    current_index: AtomicUsize,
    strategy: LoadBalancingStrategy,
    node_metrics: Arc<RwLock<Vec<NodeMetrics>>>, // ✅ ДОБАВЛЕНО: метрики узлов
}

#[derive(Debug, Clone)]
pub struct NodeMetrics {
    pub node_url: String,
    pub connection_count: usize,
    pub average_response_time: Duration,
    pub error_count: u64,
    pub last_health_check: Instant,
    pub is_healthy: bool,
}

impl ReadReplicaBalancer {
    pub fn new(replicas: Vec<DatabaseNode>) -> Self {
        let node_metrics: Vec<NodeMetrics> = replicas.iter()
            .map(|node| NodeMetrics {
                node_url: node.url.clone(),
                connection_count: 0,
                average_response_time: Duration::default(),
                error_count: 0,
                last_health_check: Instant::now(),
                is_healthy: true,
            })
            .collect();

        Self {
            replicas: Arc::new(RwLock::new(replicas)),
            current_index: AtomicUsize::new(0),
            strategy: LoadBalancingStrategy::RoundRobin,
            node_metrics: Arc::new(RwLock::new(node_metrics)),
        }
    }

    pub async fn select_replica(&self) -> Option<DatabaseNode> {
        let replicas = self.replicas.read().await;
        if replicas.is_empty() {
            return None;
        }

        match self.strategy {
            LoadBalancingStrategy::RoundRobin => {
                let index = self.current_index.fetch_add(1, Ordering::Relaxed);
                replicas.get(index % replicas.len()).cloned()
            }
            LoadBalancingStrategy::LeastConnections => {
                self.select_least_loaded(&replicas).await
            }
            LoadBalancingStrategy::LatencyBased => {
                self.select_lowest_latency(&replicas).await
            }
            LoadBalancingStrategy::WeightedRoundRobin => {
                self.select_weighted(&replicas).await
            }
        }
    }

    // ✅ ДОБАВЛЕНО: Выбор наименее нагруженной реплики
    async fn select_least_loaded(&self, replicas: &[DatabaseNode]) -> Option<DatabaseNode> {
        let metrics = self.node_metrics.read().await;
        let mut best_node = None;
        let mut min_connections = usize::MAX;

        for (i, node) in replicas.iter().enumerate() {
            if let Some(metric) = metrics.get(i) {
                if metric.is_healthy && metric.connection_count < min_connections {
                    min_connections = metric.connection_count;
                    best_node = Some(node.clone());
                }
            }
        }

        best_node
    }

    // ✅ ДОБАВЛЕНО: Выбор реплики с наименьшей задержкой
    async fn select_lowest_latency(&self, replicas: &[DatabaseNode]) -> Option<DatabaseNode> {
        let metrics = self.node_metrics.read().await;
        let mut best_node = None;
        let mut min_latency = Duration::MAX;

        for (i, node) in replicas.iter().enumerate() {
            if let Some(metric) = metrics.get(i) {
                if metric.is_healthy && metric.average_response_time < min_latency {
                    min_latency = metric.average_response_time;
                    best_node = Some(node.clone());
                }
            }
        }

        best_node
    }

    // ✅ ДОБАВЛЕНО: Взвешенный round-robin
    async fn select_weighted(&self, replicas: &[DatabaseNode]) -> Option<DatabaseNode> {
        let metrics = self.node_metrics.read().await;
        let total_weight: u32 = replicas.iter()
            .enumerate()
            .filter_map(|(i, _)| metrics.get(i).map(|m| if m.is_healthy { 1 } else { 0 }))
            .sum();

        if total_weight == 0 {
            return None;
        }

        let index = self.current_index.fetch_add(1, Ordering::Relaxed) % total_weight as usize;
        let mut current = 0;

        for (i, node) in replicas.iter().enumerate() {
            if let Some(metric) = metrics.get(i) {
                if metric.is_healthy {
                    if current == index {
                        return Some(node.clone());
                    }
                    current += 1;
                }
            }
        }

        None
    }

    // ✅ ДОБАВЛЕНО: Обновление метрик узла
    pub async fn update_node_metrics(&self, node_url: &str, response_time: Duration, success: bool) {
        let mut metrics = self.node_metrics.write().await;
        if let Some(metric) = metrics.iter_mut().find(|m| m.node_url == node_url) {
            metric.connection_count = metric.connection_count.saturating_sub(1);
            metric.average_response_time = Self::calculate_moving_average(
                metric.average_response_time,
                response_time,
                0.1 // smoothing factor
            );
            metric.last_health_check = Instant::now();

            if !success {
                metric.error_count += 1;
            }

            // Помечаем узел как нездоровый при слишком большом количестве ошибок
            if metric.error_count > 10 {
                metric.is_healthy = false;
            }
        }
    }

    fn calculate_moving_average(current: Duration, new: Duration, alpha: f64) -> Duration {
        let current_ms = current.as_millis() as f64;
        let new_ms = new.as_millis() as f64;
        let avg_ms = current_ms * (1.0 - alpha) + new_ms * alpha;
        Duration::from_millis(avg_ms as u64)
    }

    // ✅ ДОБАВЛЕНО: Добавление новой реплики
    pub async fn add_replica(&self, replica: DatabaseNode) {
        let mut replicas = self.replicas.write().await;
        replicas.push(replica.clone());

        let mut metrics = self.node_metrics.write().await;
        metrics.push(NodeMetrics {
            node_url: replica.url,
            connection_count: 0,
            average_response_time: Duration::default(),
            error_count: 0,
            last_health_check: Instant::now(),
            is_healthy: true,
        });
    }

    // ✅ ДОБАВЛЕНО: Удаление реплики
    pub async fn remove_replica(&self, node_url: &str) {
        let mut replicas = self.replicas.write().await;
        replicas.retain(|node| node.url != node_url);

        let mut metrics = self.node_metrics.write().await;
        metrics.retain(|metric| metric.node_url != node_url);
    }

    // ✅ ДОБАВЛЕНО: Получение статистики балансировщика
    pub async fn get_stats(&self) -> BalancerStats {
        let replicas = self.replicas.read().await;
        let metrics = self.node_metrics.read().await;

        let healthy_nodes = metrics.iter().filter(|m| m.is_healthy).count();
        let total_connections: usize = metrics.iter().map(|m| m.connection_count).sum();

        BalancerStats {
            total_nodes: replicas.len(),
            healthy_nodes,
            total_connections,
            strategy: self.strategy.clone(),
        }
    }

    // ✅ ДОБАВЛЕНО: Изменение стратегии балансировки
    pub fn set_strategy(&mut self, strategy: LoadBalancingStrategy) {
        self.strategy = strategy;
    }
}

#[derive(Debug)]
pub struct BalancerStats {
    pub total_nodes: usize,
    pub healthy_nodes: usize,
    pub total_connections: usize,
    pub strategy: LoadBalancingStrategy,
}