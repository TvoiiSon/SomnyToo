use super::traffic_stats::{TrafficStats, IPStats};
use crate::core::protocol::server::security::congestion_control::types::AnomalyScore;

#[derive(Debug, Clone, PartialEq)]
pub enum AnomalyType {
    HighPacketRate,     // Слишком высокая частота пакетов
    BurstTraffic,       // Резкий всплеск трафика
    SmallPacketFlood,   // Много мелких пакетов (симптом DDoS)
    ConnectionFlood,    // Много соединений с одного IP
    UnusualPacketSize,  // Необычный размер пакетов
    GeographicAnomaly,  // Аномальная география (если будем реализовывать)
}

pub struct AnomalyDetector {
    thresholds: DetectionThresholds,
}

#[derive(Debug, Clone)]
pub struct DetectionThresholds {
    pub max_packets_per_second: f64,      // Макс. пакетов/сек на IP
    pub max_connections_per_minute: u32,  // Макс. соединений/минуту на IP
    pub max_burst_multiplier: f64,        // Во сколько раз burst превышает среднее
    pub small_packet_ratio_threshold: f64,// Порог для мелких пакетов
}

impl Default for DetectionThresholds {
    fn default() -> Self {
        Self {
            max_packets_per_second: 1000.0,    // 1000 пакетов/сек
            max_connections_per_minute: 10,     // 10 соединений/минуту
            max_burst_multiplier: 5.0,         // В 5 раз выше среднего
            small_packet_ratio_threshold: 0.8, // 80% мелких пакетов
        }
    }
}

impl AnomalyDetector {
    pub fn new(thresholds: DetectionThresholds) -> Self {
        Self { thresholds }
    }

    pub fn detect_anomalies(&self, ip_stats: &IPStats, global_stats: &TrafficStats) -> Vec<(AnomalyType, f64)> {
        let mut anomalies = Vec::new();

        // 1. Проверка частоты пакетов
        if ip_stats.packet_rate > self.thresholds.max_packets_per_second {
            let severity = (ip_stats.packet_rate / self.thresholds.max_packets_per_second).min(1.0);
            anomalies.push((AnomalyType::HighPacketRate, severity));
        }

        // 2. Проверка burst-трафика
        if global_stats.packets_per_second > 0.0 {
            let burst_ratio = ip_stats.packet_rate / global_stats.packets_per_second;
            if burst_ratio > self.thresholds.max_burst_multiplier {
                let severity = (burst_ratio / self.thresholds.max_burst_multiplier).min(1.0);
                anomalies.push((AnomalyType::BurstTraffic, severity));
            }
        }

        // 3. Проверка мелких пакетов (симптом DDoS)
        if ip_stats.avg_packet_size < 100.0 { // Пакеты меньше 100 байт
            let small_packet_ratio = 1.0 - (ip_stats.avg_packet_size / 100.0).min(1.0);
            if small_packet_ratio > self.thresholds.small_packet_ratio_threshold {
                anomalies.push((AnomalyType::SmallPacketFlood, small_packet_ratio));
            }
        }

        // 4. Проверка flood соединений
        if ip_stats.connection_count > self.thresholds.max_connections_per_minute {
            let severity = (ip_stats.connection_count as f64 / self.thresholds.max_connections_per_minute as f64).min(1.0);
            anomalies.push((AnomalyType::ConnectionFlood, severity));
        }

        anomalies
    }

    pub fn calculate_anomaly_score(&self, anomalies: &[(AnomalyType, f64)]) -> AnomalyScore {
        if anomalies.is_empty() {
            return AnomalyScore {
                score: 0.0,
                reasons: vec!["normal_traffic".to_string()],
            };
        }

        // Взвешенная сумма аномалий
        let total_score: f64 = anomalies.iter()
            .map(|(anomaly_type, severity)| {
                let weight = match anomaly_type {
                    AnomalyType::HighPacketRate => 0.4,
                    AnomalyType::BurstTraffic => 0.3,
                    AnomalyType::SmallPacketFlood => 0.6, // Высокий вес - типичный DDoS
                    AnomalyType::ConnectionFlood => 0.5,
                    AnomalyType::UnusualPacketSize => 0.2,
                    AnomalyType::GeographicAnomaly => 0.3,
                };
                severity * weight
            })
            .sum();

        let normalized_score = total_score.min(1.0);

        let reasons: Vec<String> = anomalies.iter()
            .map(|(anomaly_type, severity)| {
                format!("{:?}:{:.2}", anomaly_type, severity)
            })
            .collect();

        AnomalyScore {
            score: normalized_score,
            reasons,
        }
    }
}