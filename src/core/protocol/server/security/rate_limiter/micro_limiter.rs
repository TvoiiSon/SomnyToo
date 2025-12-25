use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::{VecDeque, HashMap};
use std::time::{Duration, Instant};

use crate::core::protocol::server::security::rate_limiter::classifier::PacketClassifier;
use crate::core::protocol::server::security::rate_limiter::core::current_time_micros;
use super::classifier::PacketPriority;

#[repr(align(64))]
pub struct CacheAlignedAtomic(AtomicU64);

pub struct StripedCounter {
    stripes: Vec<CacheAlignedAtomic>,
    stripe_count: usize,
}

impl StripedCounter {
    pub fn new(stripe_count: usize) -> Self {
        let stripes = (0..stripe_count)
            .map(|_| CacheAlignedAtomic::new(0))
            .collect();
        Self { stripes, stripe_count }
    }

    #[inline(always)]
    pub fn increment(&self, key: u64) -> u64 {
        let stripe_idx = (key as usize) % self.stripe_count;
        self.stripes[stripe_idx].fetch_add(1, Ordering::Relaxed)
    }

    #[inline(always)]
    pub fn get(&self, key: u64) -> u64 {
        let stripe_idx = (key as usize) % self.stripe_count;
        self.stripes[stripe_idx].load(Ordering::Relaxed)
    }
}

impl CacheAlignedAtomic {
    pub fn new(value: u64) -> Self {
        Self(AtomicU64::new(value))
    }

    #[inline(always)]
    pub fn load(&self, ordering: Ordering) -> u64 {
        self.0.load(ordering)
    }

    #[inline(always)]
    pub fn store(&self, value: u64, ordering: Ordering) {
        self.0.store(value, ordering);
    }

    #[inline(always)]
    pub fn fetch_add(&self, value: u64, ordering: Ordering) -> u64 {
        self.0.fetch_add(value, ordering)
    }

    #[inline(always)]
    pub fn compare_exchange(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        self.0.compare_exchange(current, new, success, failure)
    }
}

pub struct HighPrecisionRateLimiter {
    // Существующие счетчики
    small_packets: Vec<CacheAlignedAtomic>,
    medium_packets: Vec<CacheAlignedAtomic>,
    large_packets: Vec<CacheAlignedAtomic>,
    control_packets: Vec<CacheAlignedAtomic>,

    // Новая система отслеживания пакетов по IP
    packet_history: std::sync::RwLock<HashMap<String, PacketHistory>>,

    // Конфигурация
    config: RateLimitConfig,
}

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub small_packets_per_second: u64,
    pub medium_packets_per_second: u64,
    pub large_packets_per_second: u64,
    pub control_packets_per_second: u64,
    pub burst_window_micros: u64,
    pub sliding_window_size: usize,        // Размер скользящего окна
    pub max_identical_packets: usize,      // Максимум одинаковых пакетов
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            small_packets_per_second: 1_000_000,
            medium_packets_per_second: 500_000,
            large_packets_per_second: 100_000,
            control_packets_per_second: 50_000,
            burst_window_micros: 50_000,
            sliding_window_size: 5,
            max_identical_packets: 100,
        }
    }
}

pub struct PacketHistory {
    pub packets: VecDeque<(Vec<u8>, Instant)>,
    pub window_size: usize,
}

impl PacketHistory {
    pub fn new(window_size: usize) -> Self {
        Self {
            packets: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    pub fn add_packet(&mut self, packet_data: &[u8]) {
        // Очищаем старые пакеты (старше 1 секунды)
        let now = Instant::now();
        while let Some((_, time)) = self.packets.front() {
            if now.duration_since(*time) > Duration::from_secs(1) {
                self.packets.pop_front();
            } else {
                break;
            }
        }

        // Добавляем новый пакет
        self.packets.push_back((packet_data.to_vec(), now));

        // Ограничиваем размер окна
        if self.packets.len() > self.window_size {
            self.packets.pop_front();
        }
    }

    pub fn count_identical_packets(&self, packet_data: &[u8]) -> usize {
        self.packets.iter()
            .filter(|(stored_packet, _)| stored_packet == packet_data)
            .count()
    }
}

impl HighPrecisionRateLimiter {
    const WINDOW_SIZE: usize = 1_000_000;

    pub fn new(config: RateLimitConfig) -> Self {
        let init_counters = || {
            (0..Self::WINDOW_SIZE)
                .map(|_| CacheAlignedAtomic::new(0))
                .collect()
        };

        Self {
            small_packets: init_counters(),
            medium_packets: init_counters(),
            large_packets: init_counters(),
            control_packets: init_counters(),
            packet_history: std::sync::RwLock::new(HashMap::new()),
            config,
        }
    }

    #[inline(always)]
    pub fn check_limit(&self, priority: PacketPriority, now_micros: u64) -> bool {
        let (counters, limit_per_second) = match priority {
            PacketPriority::HandshakeCritical => (&self.control_packets, self.config.control_packets_per_second),
            PacketPriority::SmallHighPriority => (&self.small_packets, self.config.small_packets_per_second),
            PacketPriority::DataTransferMedium => (&self.medium_packets, self.config.medium_packets_per_second),
            PacketPriority::LargeBackground => (&self.large_packets, self.config.large_packets_per_second),
            PacketPriority::HeartbeatLow => (&self.control_packets, self.config.control_packets_per_second),
        };

        let bucket_idx = (now_micros % Self::WINDOW_SIZE as u64) as usize;
        let current_count = counters[bucket_idx].fetch_add(1, Ordering::Relaxed);

        // Увеличиваем бёрст-лимит для большей терпимости
        let burst_limit = limit_per_second * self.config.burst_window_micros / 1_000_000;
        current_count < burst_limit
    }

    // Улучшенный метод проверки пакета
    pub fn check_packet(&self, ip: &str, packet_data: &[u8]) -> bool {
        let classifier = PacketClassifier::default();
        let priority = classifier.classify(packet_data);
        let now_micros = current_time_micros();

        // Базовая проверка лимита по приоритету
        if !self.check_limit(priority, now_micros) {
            return false;
        }

        // Дополнительная проверка на идентичные пакеты
        self.check_identical_packets(ip, packet_data)
    }

    fn check_identical_packets(&self, ip: &str, packet_data: &[u8]) -> bool {
        let mut history_map = self.packet_history.write().unwrap();

        let history = history_map.entry(ip.to_string())
            .or_insert_with(|| PacketHistory::new(self.config.sliding_window_size));

        // Проверяем, не слишком ли много одинаковых пакетов
        let identical_count = history.count_identical_packets(packet_data);
        if identical_count >= self.config.max_identical_packets {
            return false; // Слишком много одинаковых пакетов
        }

        // Добавляем пакет в историю
        history.add_packet(packet_data);
        true
    }

    // Метод для очистки старой истории
    pub fn cleanup_old_history(&self) {
        let mut history_map = self.packet_history.write().unwrap();
        let now = Instant::now();

        history_map.retain(|_, history| {
            // Удаляем историю, если в ней нет пакетов или все пакеты старше 5 секунд
            history.packets.iter()
                .any(|(_, time)| now.duration_since(*time) < Duration::from_secs(5))
        });
    }
}