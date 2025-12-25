use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

// Константы для ограничений
const MAX_PACKET_SIZE: usize = 10 * 1024 * 1024; // 10MB
const MAX_PACKETS_PER_IP: u64 = 1_000_000_000; // 1 миллиард пакетов
const MAX_TOTAL_PACKETS: u64 = 10_000_000_000; // 10 миллиардов пакетов
const MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024 * 1024; // 100GB

#[derive(Debug, Clone)]
pub struct TrafficStats {
    pub total_packets: u64,
    pub total_bytes: u64,
    pub packets_per_second: f64,
    pub bytes_per_second: f64,
    pub avg_packet_size: f64,
    pub connection_count: usize,
    pub start_time: Instant,
}

impl Default for TrafficStats {
    fn default() -> Self {
        Self {
            total_packets: 0,
            total_bytes: 0,
            packets_per_second: 0.0,
            bytes_per_second: 0.0,
            avg_packet_size: 0.0,
            connection_count: 0,
            start_time: Instant::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IPStats {
    pub ip: IpAddr,
    pub packet_count: u64,
    pub byte_count: u64,
    pub first_seen: Instant,
    pub last_seen: Instant,
    pub connection_count: u32,
    pub avg_packet_size: f64,
    pub packet_rate: f64,
}

pub struct TrafficStatistics {
    global_stats: RwLock<TrafficStats>,
    ip_stats: RwLock<HashMap<IpAddr, IPStats>>,
    time_windows: RwLock<Vec<(Instant, u64)>>,
}

impl TrafficStatistics {
    pub fn new() -> Self {
        Self {
            global_stats: RwLock::new(TrafficStats::default()),
            ip_stats: RwLock::new(HashMap::new()),
            time_windows: RwLock::new(Vec::new()),
        }
    }

    pub async fn record_packet(&self, ip: IpAddr, packet_size: usize) -> Result<(), String> {
        // Валидация размера пакета
        if packet_size == 0 {
            return Err("Packet size cannot be zero".to_string());
        }
        if packet_size > MAX_PACKET_SIZE {
            return Err(format!("Packet size too large: {} > {}", packet_size, MAX_PACKET_SIZE));
        }

        let now = Instant::now();

        // Обновляем глобальную статистику с проверкой переполнения
        {
            let mut stats = self.global_stats.write().await;

            // Проверка переполнения total_packets
            stats.total_packets = stats.total_packets.checked_add(1)
                .ok_or_else(|| "Total packets counter overflow".to_string())?;

            if stats.total_packets > MAX_TOTAL_PACKETS {
                return Err("Total packets limit exceeded".to_string());
            }

            // Проверка переполнения total_bytes
            stats.total_bytes = stats.total_bytes.checked_add(packet_size as u64)
                .ok_or_else(|| "Total bytes counter overflow".to_string())?;

            if stats.total_bytes > MAX_TOTAL_BYTES {
                return Err("Total bytes limit exceeded".to_string());
            }

            // Безопасное вычисление среднего размера
            stats.avg_packet_size = if stats.total_packets > 0 {
                stats.total_bytes as f64 / stats.total_packets as f64
            } else {
                0.0
            };
        }

        // Обновляем статистику по IP с проверкой лимитов
        {
            let mut ip_stats_map = self.ip_stats.write().await;
            let ip_stats = ip_stats_map.entry(ip).or_insert(IPStats {
                ip,
                packet_count: 0,
                byte_count: 0,
                first_seen: now,
                last_seen: now,
                connection_count: 0,
                avg_packet_size: 0.0,
                packet_rate: 0.0,
            });

            // Проверка лимита пакетов на IP
            if ip_stats.packet_count >= MAX_PACKETS_PER_IP {
                return Err(format!("Packet limit exceeded for IP: {}", ip));
            }

            ip_stats.packet_count = ip_stats.packet_count.checked_add(1)
                .ok_or_else(|| format!("Packet count overflow for IP: {}", ip))?;

            ip_stats.byte_count = ip_stats.byte_count.checked_add(packet_size as u64)
                .ok_or_else(|| format!("Byte count overflow for IP: {}", ip))?;

            ip_stats.last_seen = now;
            ip_stats.avg_packet_size = if ip_stats.packet_count > 0 {
                ip_stats.byte_count as f64 / ip_stats.packet_count as f64
            } else {
                0.0
            };
        }

        // Обновляем sliding window для расчета PPS
        self.update_time_window(now).await;
        self.calculate_rates().await;

        Ok(())
    }

    pub async fn record_connection(&self, ip: IpAddr) -> Result<(), String> {
        let mut ip_stats_map = self.ip_stats.write().await;

        if let Some(stats) = ip_stats_map.get_mut(&ip) {
            stats.connection_count = stats.connection_count.checked_add(1)
                .ok_or_else(|| format!("Connection count overflow for IP: {}", ip))?;
        }

        let mut global_stats = self.global_stats.write().await;
        global_stats.connection_count = global_stats.connection_count.checked_add(1)
            .ok_or("Global connection count overflow".to_string())?;

        Ok(())
    }

    // Остальные методы остаются без изменений, но добавляем проверки переполнения...
    pub async fn get_global_stats(&self) -> TrafficStats {
        self.global_stats.read().await.clone()
    }

    pub async fn get_ip_stats(&self, ip: IpAddr) -> Option<IPStats> {
        self.ip_stats.read().await.get(&ip).cloned()
    }

    pub async fn get_top_ips_by_packets(&self, limit: usize) -> Vec<IPStats> {
        let ip_stats = self.ip_stats.read().await;
        let mut stats: Vec<IPStats> = ip_stats.values().cloned().collect();
        stats.sort_by(|a, b| b.packet_count.cmp(&a.packet_count));
        stats.truncate(limit);
        stats
    }

    async fn update_time_window(&self, now: Instant) {
        let mut windows = self.time_windows.write().await;
        windows.push((now, 1));

        // Удаляем старые записи (окно 10 секунд)
        let window_duration = Duration::from_secs(10);
        windows.retain(|(time, _)| now.duration_since(*time) <= window_duration);
    }

    async fn calculate_rates(&self) {
        let windows = self.time_windows.read().await;
        let now = Instant::now();
        let window_duration = Duration::from_secs(10);

        let recent_packets: u64 = windows.iter()
            .filter(|(time, _)| now.duration_since(*time) <= window_duration)
            .map(|(_, count)| count)
            .sum();

        let pps = recent_packets as f64 / window_duration.as_secs_f64();

        let mut global_stats = self.global_stats.write().await;
        global_stats.packets_per_second = pps;
        global_stats.bytes_per_second = pps * global_stats.avg_packet_size;
    }
}