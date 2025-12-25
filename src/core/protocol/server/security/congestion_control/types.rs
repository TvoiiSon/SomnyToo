use std::net::IpAddr;
use std::time::{Duration, Instant};
use crate::core::protocol::server::security::rate_limiter::classifier::PacketPriority;

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Accept,
    RateLimit(Duration),
    TempBan(Duration),
    PermanentBan,
}

#[derive(Debug, Clone)]
pub struct PacketInfo {
    pub source_ip: IpAddr,
    pub size: usize,
    pub priority: PacketPriority,
    pub timestamp: Instant,
    pub session_id: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct ConnectionMetrics {
    pub total_packets: u64,
    pub total_bytes: u64,
    pub packets_per_second: f64,
    pub avg_packet_size: f64,
    pub connection_duration: Duration,
}

impl Default for ConnectionMetrics {
    fn default() -> Self {
        Self {
            total_packets: 0,
            total_bytes: 0,
            packets_per_second: 0.0,
            avg_packet_size: 0.0,
            connection_duration: Duration::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash,)]
pub enum LoadLevel {
    Normal,
    High,
    Critical,
    UnderAttack,
}

#[derive(Debug, Clone)]
pub struct ReputationScore {
    pub score: f64,
    pub offenses: u32,
    pub last_offense: Option<Instant>,
    pub is_whitelisted: bool,
    pub is_blacklisted: bool,
}

#[derive(Debug, Clone)]
pub struct AnomalyScore {
    pub score: f64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OffenseSeverity {
    Minor,
    Moderate,
    Major,
    Critical,
}