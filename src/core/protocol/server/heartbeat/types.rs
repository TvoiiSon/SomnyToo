use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::{info, warn, debug};
use std::net::SocketAddr;

use crate::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use crate::core::monitoring::unified_monitor::{UnifiedMonitor, AlertLevel};
use crate::core::protocol::phantom_crypto::keys::PhantomSession;

pub enum HeartbeatCommand {
    StartHeartbeat {
        session_id: Vec<u8>,
        session: Arc<PhantomSession>,
        addr: SocketAddr,
        response_tx: mpsc::UnboundedSender<Vec<u8>>,
    },
    StopHeartbeat {
        session_id: Vec<u8>,
    },
    HeartbeatReceived {
        session_id: Vec<u8>,
    },
}

pub struct ConnectionHeartbeatManager {
    session_manager: Arc<PhantomSessionManager>,
    command_tx: mpsc::UnboundedSender<HeartbeatCommand>,
    monitor: Arc<UnifiedMonitor>,
}

impl ConnectionHeartbeatManager {
    pub fn new(
        session_manager: Arc<PhantomSessionManager>,
        monitor: Arc<UnifiedMonitor>,
    ) -> Self {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        let instance = Self {
            session_manager: Arc::clone(&session_manager),
            command_tx,
            monitor: Arc::clone(&monitor),
        };

        // Запускаем фоновую задачу
        let session_manager_clone = Arc::clone(&session_manager);
        let monitor_clone = Arc::clone(&monitor);

        tokio::spawn(async move {
            let mut active_connections = std::collections::HashMap::new();
            let mut heartbeat_interval = interval(Duration::from_secs(30));

            loop {
                tokio::select! {
                    Some(command) = command_rx.recv() => {
                        Self::handle_command(
                            &mut active_connections,
                            command,
                            &session_manager_clone,
                            &monitor_clone
                        ).await;
                    }
                    _ = heartbeat_interval.tick() => {
                        Self::send_heartbeats(
                            &mut active_connections,
                            &session_manager_clone,
                        ).await;
                    }
                }
            }
        });

        instance
    }

    pub fn get_session_manager(&self) -> &Arc<PhantomSessionManager> {
        &self.session_manager
    }

    pub fn get_monitor(&self) -> &Arc<UnifiedMonitor> {
        &self.monitor
    }

    pub async fn get_stats(&self) -> HeartbeatManagerStats {
        let active_sessions = self.session_manager.get_active_sessions().await.len();
        let recent_alerts = self.monitor.get_recent_alerts(100).await.len();

        HeartbeatManagerStats {
            active_sessions,
            monitor_alerts: recent_alerts,
        }
    }

    pub async fn health_check(&self) -> bool {
        // Проверяем активные сессии через фантомный менеджер
        let sessions = self.session_manager.get_active_sessions().await;
        let sessions_healthy = !sessions.is_empty() || true; // Можно добавить более сложную логику

        let monitor_healthy = self.monitor.critical_health_check().await;

        sessions_healthy && monitor_healthy
    }

    pub async fn send_custom_alert(&self, level: AlertLevel, source: &str, message: &str) {
        self.monitor.add_alert(level, source, message).await;
    }

    pub async fn get_health_report(&self) -> serde_json::Value {
        self.monitor.generate_web_report().await
    }

    async fn handle_command(
        active_connections: &mut std::collections::HashMap<Vec<u8>, (Arc<PhantomSession>, mpsc::UnboundedSender<Vec<u8>>)>,
        command: HeartbeatCommand,
        session_manager: &Arc<PhantomSessionManager>,
        monitor: &Arc<UnifiedMonitor>,
    ) {
        match command {
            HeartbeatCommand::StartHeartbeat { session_id, session, addr, response_tx } => {
                active_connections.insert(session_id.clone(), (session.clone(), response_tx));

                session_manager.register_session(
                    session_id.clone(),
                    session,
                    addr
                ).await;

                monitor.add_alert(
                    AlertLevel::Info,
                    "heartbeat",
                    &format!("Heartbeat started for phantom session: {} from {}",
                             hex::encode(&session_id), addr)
                ).await;

                info!("👻 Phantom heartbeat started for session: {} from {}",
                     hex::encode(&session_id), addr);
            }
            HeartbeatCommand::StopHeartbeat { session_id } => {
                active_connections.remove(&session_id);
                session_manager.force_remove_session(&session_id).await;

                info!("👻 Phantom heartbeat stopped for session: {}",
                     hex::encode(&session_id));
            }
            HeartbeatCommand::HeartbeatReceived { session_id } => {
                session_manager.on_heartbeat_received(&session_id).await;

                info!("👻 Phantom heartbeat received for session: {}",
                     hex::encode(&session_id));
            }
        }
    }

    async fn send_heartbeats(
        active_connections: &mut std::collections::HashMap<Vec<u8>, (Arc<PhantomSession>, mpsc::UnboundedSender<Vec<u8>>)>,
        session_manager: &Arc<PhantomSessionManager>,
    ) {
        let mut to_remove = Vec::new();

        // TODO
        for (session_id, (_session, response_tx)) in active_connections.iter() {
            // Временное решение: создаем простой heartbeat пакет
            let heartbeat_data = vec![0x11]; // Простой heartbeat пакет

            if response_tx.send(heartbeat_data).is_err() {
                warn!("👻 Failed to send heartbeat to phantom session: {}, connection closed",
                     hex::encode(session_id));
                to_remove.push(session_id.clone());
            } else {
                // Обновляем время последней активности
                session_manager.update_activity(session_id).await;
                debug!("👻 Phantom heartbeat sent to session: {}",
                     hex::encode(session_id));
            }
        }

        for session_id in to_remove {
            active_connections.remove(&session_id);
            session_manager.force_remove_session(&session_id).await;
        }
    }

    pub fn start_heartbeat(
        &self,
        session_id: Vec<u8>,
        session: Arc<PhantomSession>,
        addr: SocketAddr,
        response_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) {
        let _ = self.command_tx.send(HeartbeatCommand::StartHeartbeat {
            session_id,
            session,
            addr,
            response_tx,
        });
    }

    pub fn stop_heartbeat(&self, session_id: Vec<u8>) {
        let _ = self.command_tx.send(HeartbeatCommand::StopHeartbeat { session_id });
    }

    pub fn heartbeat_received(&self, session_id: Vec<u8>) {
        let _ = self.command_tx.send(HeartbeatCommand::HeartbeatReceived { session_id });
    }

    pub async fn validate_license(&self, _session_id: &[u8]) -> bool {
        // Заглушка для проверки лицензии
        true
    }

    pub async fn link_license_to_session(&self, _session_id: Vec<u8>, _license_key: String) {
        info!("👻 License linked to phantom session (stub implementation)");
    }

    // Дополнительный метод для получения информации о сессии
    pub async fn get_session_info(&self, session_id: &[u8]) -> Option<SessionInfo> {
        if let Some(session) = self.session_manager.get_session(session_id).await {
            Some(SessionInfo {
                session_id: hex::encode(session_id),
                is_valid: session.is_valid(),
                created_at: std::time::Instant::now(), // Нужно добавить это поле в PhantomSession
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct HeartbeatManagerStats {
    pub active_sessions: usize,
    pub monitor_alerts: usize,
}

// Структура для информации о сессии
pub struct SessionInfo {
    pub session_id: String,
    pub is_valid: bool,
    pub created_at: std::time::Instant,
}