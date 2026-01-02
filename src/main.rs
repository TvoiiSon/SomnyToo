use tokio::net::TcpListener;
use std::sync::Arc;
use anyhow::Result;
use tracing_subscriber::{FmtSubscriber, EnvFilter};
use tracing::{info, error, warn};

use somnytoo::config::{AppConfig, ServerConfig, PhantomConfig};
use somnytoo::core::protocol::server::tcp_server_phantom::handle_phantom_connection;
use somnytoo::core::protocol::server::session_manager_phantom::PhantomSessionManager;
use somnytoo::core::protocol::server::connection_manager_phantom::PhantomConnectionManager;
use somnytoo::core::protocol::crypto::crypto_pool_phantom::PhantomCryptoPool;

// Импортируем heartbeat компоненты
use somnytoo::core::protocol::server::heartbeat::manager::{HeartbeatManager, HeartbeatConfig};
use somnytoo::core::protocol::server::heartbeat::sender::HeartbeatSender;
use somnytoo::core::protocol::server::heartbeat::types::ConnectionHeartbeatManager;

#[tokio::main]
async fn main() -> Result<()> {
    let app_config = AppConfig::from_env();

    if let Err(e) = app_config.validate() {
        error!("❌ Invalid configuration: {}", e);
        std::process::exit(1);
    }

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&app_config.log_level));

    let subscriber = FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_target(true)
        .with_level(true)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    info!("🚀 Starting Server Mode...");

    info!("📝 Configuration loaded:");
    info!("  - Host: {}", app_config.server.host);
    info!("  - Port: {}", app_config.server.port);
    info!("  - Log level: {}", app_config.log_level);
    info!("  - Database URL: {}", app_config.database.primary_url);
    info!("  - Phantom Mode: {}", app_config.phantom.enabled);
    info!("  - Phantom Assembler: {}", app_config.phantom.assembler_type);
    info!("  - Hardware Auth: {}", app_config.phantom.hardware_auth_enabled);

    run_server_mode(app_config).await
}

async fn run_server_mode(app_config: AppConfig) -> Result<()> {
    info!("🚀 Initializing phantom security server...");

    // Инициализация базы данных
    initialize_database(app_config.database.clone()).await;

    // Инициализация фантомных компонентов
    let phantom_connection_manager = Arc::new(PhantomConnectionManager::new());
    let phantom_session_manager = Arc::new(PhantomSessionManager::new(
        phantom_connection_manager.clone()
    ));

    // Инициализация криптопулла
    let phantom_crypto_pool = Arc::new(PhantomCryptoPool::spawn(
        num_cpus::get() // Используем количество ядер CPU
    ));

    // Инициализация Heartbeat системы
    info!("💓 Initializing heartbeat system...");
    let heartbeat_system = initialize_heartbeat_system(
        phantom_session_manager.clone(),
        phantom_connection_manager.clone(),
    ).await;

    info!("💓 Heartbeat system initialized successfully");

    info!("🎯 Server is ready and accepting phantom connections");

    // Запуск сервера
    start_phantom_server(
        app_config.server,
        app_config.phantom,
        phantom_session_manager,
        phantom_connection_manager,
        phantom_crypto_pool,
        heartbeat_system, // Передаем heartbeat систему
    ).await
}

async fn initialize_heartbeat_system(
    session_manager: Arc<PhantomSessionManager>,
    connection_manager: Arc<PhantomConnectionManager>,
) -> Arc<ConnectionHeartbeatManager> {
    use somnytoo::core::monitoring::unified_monitor::UnifiedMonitor;
    use somnytoo::core::monitoring::config::MonitoringConfig;

    // Создаем конфигурацию мониторинга
    let monitoring_config = MonitoringConfig::default();
    let monitor = Arc::new(UnifiedMonitor::new(monitoring_config));

    // Создаем менеджер heartbeat для соединений
    let connection_heartbeat_manager = Arc::new(ConnectionHeartbeatManager::new(
        session_manager,
        monitor,
    ));

    // Создаем конфигурацию heartbeat
    let heartbeat_config = HeartbeatConfig {
        ping_interval: std::time::Duration::from_secs(30),
        timeout: std::time::Duration::from_secs(60),
        max_missed_pings: 3,
    };

    // Создаем основной менеджер heartbeat
    let heartbeat_manager = Arc::new(HeartbeatManager::new(
        heartbeat_config,
        connection_manager.clone(),
    ));

    // Запускаем основной heartbeat manager
    heartbeat_manager.start().await;
    info!("✅ Basic heartbeat manager started");

    // Создаем и запускаем heartbeat sender
    let heartbeat_sender = Arc::new(HeartbeatSender::new(heartbeat_manager.clone()));
    heartbeat_sender.clone().start().await;
    info!("✅ Heartbeat sender started");

    // Можно вернуть connection heartbeat manager для использования в других частях системы
    connection_heartbeat_manager
}

async fn start_phantom_server(
    server_config: ServerConfig,
    phantom_config: PhantomConfig,
    session_manager: Arc<PhantomSessionManager>,
    connection_manager: Arc<PhantomConnectionManager>,
    crypto_pool: Arc<PhantomCryptoPool>,
    heartbeat_manager: Arc<ConnectionHeartbeatManager>, // Добавляем параметр
) -> Result<()> {
    let addr = server_config.get_addr();
    let listener = TcpListener::bind(&addr).await?;

    info!(target: "server", "👻 Phantom Security Server listening on {}", addr);
    info!("🔧 Phantom Configuration:");
    info!("  - Session timeout: {}ms", phantom_config.session_timeout_ms);
    info!("  - Max sessions: {}", phantom_config.max_sessions);
    info!("  - Hardware acceleration: {}", phantom_config.enable_hardware_acceleration);
    info!("  - Constant time enforcement: {}", phantom_config.constant_time_enforced);
    info!("  - Assembler type: {}", phantom_config.assembler_type);

    // Выводим информацию о heartbeat системе
    let stats = heartbeat_manager.get_stats().await;
    info!("💓 Heartbeat System:");
    info!("  - Active sessions: {}", stats.active_sessions);
    info!("  - Monitor alerts: {}", stats.monitor_alerts);

    loop {
        let (stream, _) = listener.accept().await?;
        let peer = stream.peer_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());

        let session_manager = session_manager.clone();
        let connection_manager = connection_manager.clone();
        let crypto_pool = crypto_pool.clone();
        let phantom_config = phantom_config.clone();
        let heartbeat_manager = heartbeat_manager.clone(); // Клонируем heartbeat manager

        tokio::spawn(async move {
            info!(target: "server", "👻 New phantom connection from {}", peer);

            match handle_phantom_connection(
                stream,
                peer,
                phantom_config,
                session_manager,
                connection_manager,
                crypto_pool,
                heartbeat_manager, // Передаем в обработчик соединения
            ).await {
                Ok(()) => {
                    info!(target: "server", "👻 Phantom connection with {} closed cleanly", peer);
                }
                Err(e) => {
                    error!(target: "server", "👻 Phantom connection {} error: {}", peer, e);
                }
            }
        });
    }
}

async fn initialize_database(db_config: somnytoo::config::DatabaseConfig) {
    use somnytoo::core::sql_server::server::SqlServer;
    use somnytoo::core::sql_server::config::{SecurityConfig, DatabaseConfig as SqlDatabaseConfig};
    use somnytoo::core::sql_server::executor::QUERY_EXECUTOR;

    if QUERY_EXECUTOR.is_initialized().await {
        info!("Database already initialized");
        return;
    }

    let sql_db_config = SqlDatabaseConfig::from_global_config(&db_config);
    let security_config = SecurityConfig::default_from_env();

    match SqlServer::new(sql_db_config, security_config).await {
        Ok(mut server) => {
            if let Err(e) = server.start().await {
                warn!("Failed to start SQL server: {}, using in-memory mode", e);
                return;
            }
            QUERY_EXECUTOR.register_server(server).await;
            info!("Database initialized successfully");
        }
        Err(e) => {
            warn!("Failed to initialize SQL server: {}, using in-memory mode", e);
        }
    }
}