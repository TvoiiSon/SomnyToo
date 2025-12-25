use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct AIMDConfig {
    pub additive_increase: u64,    // Сколько добавлять при успехе
    pub multiplicative_decrease: f64, // На сколько умножать при перегрузке (0.5 = уменьшить вдвое)
    pub initial_limit: u64,        // Начальный лимит
    pub max_limit: u64,            // Максимальный лимит
    pub min_limit: u64,            // Минимальный лимит
    pub check_interval: Duration,  // Как часто проверять
}

impl Default for AIMDConfig {
    fn default() -> Self {
        Self {
            additive_increase: 10,
            multiplicative_decrease: 0.7, // Уменьшить на 30%
            initial_limit: 100,
            max_limit: 10_000,
            min_limit: 10,
            check_interval: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IPLimitState {
    pub current_limit: u64,
    pub used_this_window: u64,
    pub last_updated: Instant,
    pub success_count: u32,
    pub failure_count: u32,
}

#[derive(Debug, Clone)]
pub struct CleanupStats {
    pub total_ips: usize,
    pub last_cleanup: Instant,
    pub time_since_last_cleanup: Duration,
}

pub struct AIMDAlgorithm {
    config: AIMDConfig,
    ip_states: RwLock<HashMap<IpAddr, IPLimitState>>,
    last_cleanup: RwLock<Instant>,
}

impl AIMDAlgorithm {
    pub fn new(config: AIMDConfig) -> Self {
        Self {
            config,
            ip_states: RwLock::new(HashMap::new()),
            last_cleanup: RwLock::new(Instant::now()),
        }
    }

    pub async fn start_periodic_cleanup(self: Arc<Self>) {
        let cleanup_interval = Duration::from_secs(300); // 5 минут

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval);

            loop {
                interval.tick().await;

                // ИСПРАВЛЕНИЕ: Обновляем last_cleanup и выполняем очистку
                {
                    let mut last_cleanup = self.last_cleanup.write().await;
                    *last_cleanup = Instant::now();
                }

                self.cleanup_old_entries().await;
            }
        });
    }

    pub async fn get_cleanup_stats(&self) -> CleanupStats {
        let states = self.ip_states.read().await;
        let last_cleanup = self.last_cleanup.read().await;

        CleanupStats {
            total_ips: states.len(),
            last_cleanup: *last_cleanup,
            time_since_last_cleanup: last_cleanup.elapsed(),
        }
    }

    // Добавляем метод для принудительной очистки
    pub async fn force_cleanup(&self) -> usize {
        let mut states = self.ip_states.write().await;
        let initial_count = states.len();
        let now = Instant::now();
        let cleanup_interval = Duration::from_secs(300);

        states.retain(|_, state| now.duration_since(state.last_updated) < cleanup_interval);

        let final_count = states.len();
        initial_count - final_count
    }

    pub async fn get_limit(&self, ip: IpAddr) -> u64 {
        let states = self.ip_states.read().await;
        states.get(&ip)
            .map(|state| state.current_limit)
            .unwrap_or(self.config.initial_limit)
    }

    pub async fn record_success(&self, ip: IpAddr) -> u64 {
        let mut states = self.ip_states.write().await;
        let state = states.entry(ip).or_insert_with(|| IPLimitState {
            current_limit: self.config.initial_limit,
            used_this_window: 0,
            last_updated: Instant::now(),
            success_count: 0,
            failure_count: 0,
        });

        state.success_count += 1;
        state.used_this_window += 1;

        // Additive Increase: увеличиваем лимит при успехе
        if state.success_count >= 10 { // Каждые 10 успехов
            state.current_limit = (state.current_limit + self.config.additive_increase)
                .min(self.config.max_limit);
            state.success_count = 0;
        }

        state.current_limit
    }

    pub async fn record_failure(&self, ip: IpAddr) -> u64 {
        let mut states = self.ip_states.write().await;
        let state = states.entry(ip).or_insert_with(|| IPLimitState {
            current_limit: self.config.initial_limit,
            used_this_window: 0,
            last_updated: Instant::now(),
            success_count: 0,
            failure_count: 0,
        });

        state.failure_count += 1;

        // Multiplicative Decrease: уменьшаем лимит при перегрузке
        if state.failure_count >= 3 { // После 3 ошибок
            state.current_limit = ((state.current_limit as f64) * self.config.multiplicative_decrease) as u64;
            state.current_limit = state.current_limit.max(self.config.min_limit);
            state.failure_count = 0;
        }

        state.current_limit
    }

    pub async fn reset_window(&self, ip: IpAddr) {
        let mut states = self.ip_states.write().await;
        if let Some(state) = states.get_mut(&ip) {
            state.used_this_window = 0;
            state.last_updated = Instant::now();
        }
    }

    pub async fn cleanup_old_entries(&self) {
        let mut states = self.ip_states.write().await;
        let now = Instant::now();
        let cleanup_interval = Duration::from_secs(300); // 5 минут

        states.retain(|_, state| now.duration_since(state.last_updated) < cleanup_interval);
    }
}