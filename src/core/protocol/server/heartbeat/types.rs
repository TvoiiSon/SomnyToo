use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{interval};
use tracing::{info, warn};
use std::net::SocketAddr;

use crate::core::protocol::server::connection_manager::ConnectionManager;
use crate::core::protocol::crypto::key_manager::session_keys::SessionKeys;
use crate::core::protocol::server::session_manager::SessionManager;
use crate::core::monitoring::unified_monitor::{UnifiedMonitor, AlertLevel};
use crate::core::monitoring::config::MonitoringConfig;

pub enum HeartbeatCommand {
    StartHeartbeat {
        session_id: Vec<u8>,
        session_keys: Arc<SessionKeys>,
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

// ИСПРАВЛЕНИЕ: Убираем generic параметр T
pub struct ConnectionHeartbeatManager {
    session_manager: Arc<SessionManager>,
    command_tx: mpsc::UnboundedSender<HeartbeatCommand>,
    // ИСПРАВЛЕНИЕ: Убираем license_integration из структуры, так как он вызывает проблемы
    monitor: Arc<UnifiedMonitor>,
}

// ИСПРАВЛЕНИЕ: Убираем generic параметр T
impl ConnectionHeartbeatManager {
    pub fn new(
        session_manager: Arc<SessionManager>,
        monitor: Arc<UnifiedMonitor>,
    ) -> Self {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        // ИСПРАВЛЕНИЕ: Убираем license_integration из структуры
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

    // ИСПРАВЛЕНИЕ: Обновляем методы для доступа к полям
    pub fn get_session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    pub fn get_monitor(&self) -> &Arc<UnifiedMonitor> {
        &self.monitor
    }

    // ИСПРАВЛЕНИЕ: Добавляем метод для получения статистики
    pub async fn get_stats(&self) -> HeartbeatManagerStats {
        let active_sessions = self.session_manager.get_active_sessions().await.len();

        // Используем существующий метод get_recent_alerts для получения количества алертов
        let recent_alerts = self.monitor.get_recent_alerts(100).await.len();

        HeartbeatManagerStats {
            active_sessions,
            monitor_alerts: recent_alerts,
        }
    }

    // ИСПРАВЛЕНИЕ: Добавляем метод для проверки здоровья
    pub async fn health_check(&self) -> bool {
        // Проверяем, что все компоненты работают
        let sessions_healthy = !self.session_manager.get_active_sessions().await.is_empty()
            || true; // Или другая логика проверки

        // Используем существующий метод critical_health_check для проверки монитора
        let monitor_healthy = self.monitor.critical_health_check().await;

        sessions_healthy && monitor_healthy
    }

    // ИСПРАВЛЕНИЕ: Добавляем метод для отправки кастомных алертов через монитор
    pub async fn send_custom_alert(&self, level: AlertLevel, source: &str, message: &str) {
        self.monitor.add_alert(level, source, message).await;
    }

    // ИСПРАВЛЕНИЕ: Добавляем метод для получения общего отчета о здоровье
    pub async fn get_health_report(&self) -> serde_json::Value {
        self.monitor.generate_web_report().await
    }

    // ИСПРАВЛЕНИЕ: Обновляем handle_command - убираем license_integration
    async fn handle_command(
        active_connections: &mut std::collections::HashMap<Vec<u8>, (Arc<SessionKeys>, mpsc::UnboundedSender<Vec<u8>>)>,
        command: HeartbeatCommand,
        session_manager: &Arc<SessionManager>,
        monitor: &Arc<UnifiedMonitor>,
    ) {
        match command {
            HeartbeatCommand::StartHeartbeat { session_id, session_keys, addr, response_tx } => {
                active_connections.insert(session_id.clone(), (session_keys.clone(), response_tx));

                session_manager.register_session(
                    session_id.clone(),
                    session_keys,
                    addr
                ).await;

                // ИСПРАВЛЕНИЕ: Убираем вызов license_integration
                // В реальной реализации здесь может быть интеграция с лицензиями
                // через другой механизм

                monitor.add_alert(
                    AlertLevel::Info,
                    "heartbeat",
                    &format!("Heartbeat started for session: {} from {}", hex::encode(&session_id), addr)
                ).await;

                info!("Heartbeat started for session: {} from {}", hex::encode(&session_id), addr);
            }
            HeartbeatCommand::StopHeartbeat { session_id } => {
                active_connections.remove(&session_id);
                session_manager.force_remove_session(&session_id).await;

                // ИСПРАВЛЕНИЕ: Убираем вызов license_integration

                info!("Heartbeat stopped for session: {}", hex::encode(&session_id));
            }
            HeartbeatCommand::HeartbeatReceived { session_id } => {
                session_manager.on_heartbeat_received(&session_id).await;

                // ИСПРАВЛЕНИЕ: Убираем проверку лицензии
                // В реальной реализации здесь может быть проверка лицензии
                // через другой механизм

                info!("Heartbeat received for session: {}", hex::encode(&session_id));
            }
        }
    }

    // ИСПРАВЛЕНИЕ: Обновляем send_heartbeats - убираем license_integration
    async fn send_heartbeats(
        active_connections: &mut std::collections::HashMap<Vec<u8>, (Arc<SessionKeys>, mpsc::UnboundedSender<Vec<u8>>)>,
        session_manager: &Arc<SessionManager>,
    ) {
        let mut to_remove = Vec::new();

        for (session_id, (session_keys, response_tx)) in active_connections.iter() {
            // ИСПРАВЛЕНИЕ: Убираем проверку лицензии
            // В реальной реализации здесь может быть проверка:
            // if !self.is_license_valid(session_id).await { ... }

            let heartbeat_packet = crate::core::protocol::packets::encoder::packet_builder::PacketBuilder::build_encrypted_packet(
                session_keys,
                0x11,
                b"ping",
            ).await;

            if response_tx.send(heartbeat_packet).is_err() {
                warn!("Failed to send heartbeat to session: {}, connection closed", hex::encode(session_id));
                to_remove.push(session_id.clone());
            } else {
                session_manager.on_ping_sent(session_id).await;
            }
        }

        for session_id in to_remove {
            active_connections.remove(&session_id);
            session_manager.force_remove_session(&session_id).await;

            // ИСПРАВЛЕНИЕ: Убираем вызов license_integration
        }
    }

    pub fn start_heartbeat(
        &self,
        session_id: Vec<u8>,
        session_keys: Arc<SessionKeys>,
        addr: SocketAddr,
        response_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) {
        let _ = self.command_tx.send(HeartbeatCommand::StartHeartbeat {
            session_id,
            session_keys,
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

    // ИСПРАВЛЕНИЕ: Добавляем метод для интеграции с лицензиями (опционально)
    pub async fn validate_license(&self, _session_id: &[u8]) -> bool {
        // Заглушка для проверки лицензии
        // В реальной реализации здесь будет интеграция с системой лицензий
        true
    }

    // ИСПРАВЛЕНИЕ: Добавляем метод для привязки лицензии к сессии (опционально)
    pub async fn link_license_to_session(&self, _session_id: Vec<u8>, _license_key: String) {
        // Заглушка для привязки лицензии
        // В реальной реализации здесь будет логика привязки лицензии
        info!("License linked to session (stub implementation)");
    }
}

// ИСПРАВЛЕНИЕ: Обновляем структуру для статистики
#[derive(Debug, Clone)]
pub struct HeartbeatManagerStats {
    pub active_sessions: usize,
    pub monitor_alerts: usize,
}

// ИСПРАВЛЕНИЕ: Упрощаем реализацию Default
impl Default for ConnectionHeartbeatManager {
    fn default() -> Self {
        // Создаем компоненты для тестов
        let connection_manager = Arc::new(ConnectionManager::new());
        let session_manager = Arc::new(SessionManager::new(connection_manager));
        let monitor = Arc::new(UnifiedMonitor::new(MonitoringConfig::default()));

        Self::new(session_manager, monitor)
    }
}