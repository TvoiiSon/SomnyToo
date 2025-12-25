use std::net::IpAddr;
use std::time::Duration;
use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::error;

use crate::core::protocol::server::security::congestion_control::traits::TrafficAnalyzer;
use crate::core::protocol::server::security::congestion_control::types::{
    PacketInfo, ConnectionMetrics, LoadLevel, AnomalyScore
};

use super::traffic_stats::TrafficStatistics;
use super::anomaly_detector::{AnomalyDetector, DetectionThresholds};

pub struct TrafficAnalyzerImpl {
    stats: TrafficStatistics,
    detector: AnomalyDetector,
    load_level: RwLock<LoadLevel>,
}

impl TrafficAnalyzerImpl {
    pub fn new() -> Self {
        Self {
            stats: TrafficStatistics::new(),
            detector: AnomalyDetector::new(DetectionThresholds::default()),
            load_level: RwLock::new(LoadLevel::Normal),
        }
    }

    pub fn with_thresholds(thresholds: DetectionThresholds) -> Self {
        Self {
            stats: TrafficStatistics::new(),
            detector: AnomalyDetector::new(thresholds),
            load_level: RwLock::new(LoadLevel::Normal),
        }
    }

    async fn update_load_level(&self) -> Result<(), String> {
        let global_stats = self.stats.get_global_stats().await;
        let pps = global_stats.packets_per_second;

        let new_level = if pps > 10000.0 {
            LoadLevel::UnderAttack
        } else if pps > 5000.0 {
            LoadLevel::Critical
        } else if pps > 1000.0 {
            LoadLevel::High
        } else {
            LoadLevel::Normal
        };

        *self.load_level.write().await = new_level;
        Ok(())
    }
}

#[async_trait]
impl TrafficAnalyzer for TrafficAnalyzerImpl {
    async fn analyze_packet(&self, packet: &PacketInfo, _metrics: &ConnectionMetrics) -> AnomalyScore {
        // Записываем статистику пакета с обработкой ошибок
        if let Err(e) = self.stats.record_packet(packet.source_ip, packet.size).await {
            // В случае ошибки возвращаем высокий anomaly score
            return AnomalyScore {
                score: 0.8,
                reasons: vec![format!("Statistics error: {}", e)]
            };
        }

        // Получаем статистику по IP
        let ip_stats = match self.stats.get_ip_stats(packet.source_ip).await {
            Some(stats) => stats,
            None => return AnomalyScore {
                score: 0.0,
                reasons: vec!["new_ip".to_string()]
            },
        };

        let global_stats = self.stats.get_global_stats().await;

        // Обнаруживаем аномалии
        let anomalies = self.detector.detect_anomalies(&ip_stats, &global_stats);
        let anomaly_score = self.detector.calculate_anomaly_score(&anomalies);

        // Обновляем уровень нагрузки (игнорируем ошибки)
        let _ = self.update_load_level().await;

        anomaly_score
    }

    async fn get_system_load(&self) -> LoadLevel {
        self.load_level.read().await.clone()
    }

    async fn on_connection_opened(&self, ip: IpAddr, _session_id: Vec<u8>) {
        if let Err(e) = self.stats.record_connection(ip).await {
            error!("Error recording connection for IP {}: {}", ip, e);
        }
        let _ = self.update_load_level().await;
    }

    async fn on_connection_closed(&self, _ip: IpAddr, _session_id: Vec<u8>) {
        let _ = self.update_load_level().await;
    }

    async fn get_ip_stats(&self, ip: IpAddr, _period: Duration) -> ConnectionMetrics {
        let ip_stats = self.stats.get_ip_stats(ip).await;

        ip_stats.map(|stats| ConnectionMetrics {
            total_packets: stats.packet_count,
            total_bytes: stats.byte_count,
            packets_per_second: stats.packet_rate,
            avg_packet_size: stats.avg_packet_size,
            connection_duration: stats.last_seen.duration_since(stats.first_seen),
        }).unwrap_or_else(|| ConnectionMetrics {
            total_packets: 0,
            total_bytes: 0,
            packets_per_second: 0.0,
            avg_packet_size: 0.0,
            connection_duration: Duration::default(),
        })
    }
}