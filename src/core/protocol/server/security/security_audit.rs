use tokio::sync::mpsc;
use tracing::{info, warn, error};
use std::net::{SocketAddr, IpAddr};
use once_cell::sync::Lazy;
use tokio::sync::Mutex;

use crate::core::protocol::server::security::security_metrics::SecurityMetrics;

#[derive(Debug)]
pub enum AuditEvent {
    HandshakeSuccess { peer: SocketAddr, session_id: [u8; 16] },
    HandshakeFailure { peer: SocketAddr, reason: String },
    ReplayAttack { peer: SocketAddr, nonce: [u8; 12] },
    RateLimitExceeded { peer: SocketAddr, count: usize },
    SessionReuse { peer: SocketAddr, session_id: [u8; 16] },
    PacketRateLimitExceeded { peer: SocketAddr, packet_size: usize, priority: String },
    CongestionDecision { ip: IpAddr, decision_type: String, reason: String },
    ConnectionEvent { ip: IpAddr, event_type: String, session_id: String },
}

pub struct SecurityAudit {
    tx: Option<mpsc::Sender<AuditEvent>>, // Теперь Option
}

// Глобальный экземпляр с ленивой инициализацией
static GLOBAL_AUDIT: Lazy<Mutex<SecurityAudit>> = Lazy::new(|| {
    Mutex::new(SecurityAudit { tx: None })
});

impl SecurityAudit {
    pub async fn initialize() -> Result<(), Box<dyn std::error::Error>> {
        let mut global_audit = GLOBAL_AUDIT.lock().await;

        if global_audit.tx.is_none() {
            let (tx, mut rx) = mpsc::channel(1000);

            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    if let Err(e) = SecurityAudit::process_event(event).await {
                        error!("Error processing audit event: {}", e);
                    }
                }
            });

            global_audit.tx = Some(tx);
            info!("SecurityAudit initialized successfully");
        }

        Ok(())
    }

    async fn process_event(event: AuditEvent) -> Result<(), Box<dyn std::error::Error>> {
        match event {
            AuditEvent::HandshakeSuccess { peer, session_id } => {
                info!(target: "security", "Handshake success: {} session: {}", peer, hex::encode(session_id));
                SecurityMetrics::successful_sessions().inc();
            }
            AuditEvent::HandshakeFailure { peer, reason } => {
                warn!(target: "security", "Handshake failed: {} reason: {}", peer, reason);
                SecurityMetrics::failed_handshakes().inc();
            }
            AuditEvent::ReplayAttack { peer, nonce } => {
                error!(target: "security", "REPLAY ATTACK detected from {} nonce: {}", peer, hex::encode(nonce));
                SecurityMetrics::replay_attacks().inc();
            }
            AuditEvent::RateLimitExceeded { peer, count } => {
                warn!(target: "security", "Rate limit exceeded: {} count: {}", peer, count);
                SecurityMetrics::rate_limit_hits().inc();
            }
            AuditEvent::SessionReuse { peer, session_id } => {
                warn!(target: "security", "Session reuse detected: {} session: {}", peer, hex::encode(session_id));
            }
            AuditEvent::PacketRateLimitExceeded { peer, packet_size, priority } => {
                warn!(target: "security", "Packet rate limit exceeded: {} size: {} priority: {}", peer, packet_size, priority);
                SecurityMetrics::rate_limit_hits().inc();
            }
            AuditEvent::CongestionDecision { ip, decision_type, reason } => {
                info!(target: "congestion", "Congestion decision: {} type: {} reason: {}", ip, decision_type, reason);
            }
            AuditEvent::ConnectionEvent { ip, event_type, session_id } => {
                info!(target: "congestion", "Connection event: {} type: {} session: {}", ip, event_type, session_id);
            }
        }
        Ok(())
    }

    async fn send_event(event: AuditEvent) -> Result<(), Box<dyn std::error::Error>> {
        let audit = GLOBAL_AUDIT.lock().await;

        if let Some(tx) = &audit.tx {
            tx.send(event).await
                .map_err(|e| format!("Failed to send audit event: {}", e).into())
        } else {
            Err("SecurityAudit not initialized".into())
        }
    }

    // Публичные методы теперь используют глобальный экземпляр
    pub async fn log_handshake_success(peer: SocketAddr, session_id: [u8; 16]) {
        if let Err(e) = Self::send_event(AuditEvent::HandshakeSuccess { peer, session_id }).await {
            error!("Failed to log handshake success: {}", e);
        }
    }

    pub async fn log_handshake_failure(peer: SocketAddr, reason: String) {
        if let Err(e) = Self::send_event(AuditEvent::HandshakeFailure { peer, reason }).await {
            error!("Failed to log handshake failure: {}", e);
        }
    }

    pub async fn log_session_reuse(peer: SocketAddr, session_id: [u8; 16]) {
        if let Err(e) = Self::send_event(AuditEvent::SessionReuse { peer, session_id }).await {
            error!("Failed to log session reuse: {}", e);
        }
    }

    pub async fn log_packet_rate_limit(peer: SocketAddr, packet_size: usize, priority: String) {
        if let Err(e) = Self::send_event(AuditEvent::PacketRateLimitExceeded {
            peer,
            packet_size,
            priority
        }).await {
            error!("Failed to log packet rate limit: {}", e);
        }
    }

    pub async fn log_congestion_decision(ip: IpAddr, decision_type: &str, reason: &str) {
        if let Err(e) = Self::send_event(AuditEvent::CongestionDecision {
            ip,
            decision_type: decision_type.to_string(),
            reason: reason.to_string()
        }).await {
            error!("Failed to log congestion decision: {}", e);
        }
    }

    pub async fn log_connection_event(ip: IpAddr, event_type: &str, session_id: &str) {
        if let Err(e) = Self::send_event(AuditEvent::ConnectionEvent {
            ip,
            event_type: event_type.to_string(),
            session_id: session_id.to_string(),
        }).await {
            error!("Failed to log connection event: {}", e);
        }
    }

    pub async fn log_rate_limit_event(ip: &str, packet_size: usize, priority: &str, allowed: bool) {
        let event_type = if allowed { "ALLOWED" } else { "BLOCKED" };
        info!(
            "Rate limit check: IP={}, size={}, priority={}, result={}",
            ip, packet_size, priority, event_type
        );
    }
}