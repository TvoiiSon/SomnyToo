use tokio::net::TcpListener;
use tokio::time::{Duration};
use std::sync::Arc;
use anyhow::Result;
use tracing_subscriber::{FmtSubscriber, EnvFilter};
use tracing::{info, error, warn};

use somnytoo::core::protocol::server::tcp_server::handle_connection;
use somnytoo::core::protocol::packets::processor::dispatcher::Dispatcher;
use somnytoo::core::protocol::packets::processor::packet_service::PacketService;
use somnytoo::core::protocol::server::session_manager::SessionManager;
use somnytoo::core::protocol::server::heartbeat::sender::HeartbeatSender;
use somnytoo::core::monitoring::monitor_registry::MonitorRegistry;
use somnytoo::core::protocol::server::connection_manager::ConnectionManager;
use somnytoo::config::{AppConfig, ServerConfig, DatabaseConfig as ConfigDatabaseConfig};

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

    run_server_mode(app_config).await
}

/// РЕЖИМ СЕРВЕРА - запускает полный сервер с мониторингом
async fn run_server_mode(app_config: AppConfig) -> Result<()> {
    info!("🚀 Initializing server with full monitoring...");

    initialize_database(app_config.database.clone()).await;

    info!("🔍 Starting comprehensive monitoring system...");
    let monitor_registry = Arc::new(MonitorRegistry::new().await);

    info!("🏥 Performing initial health check (with timeout)...");
    let health_check_result = tokio::time::timeout(
        Duration::from_secs(30),
        monitor_registry.health_check()
    ).await;

    match health_check_result {
        Ok(health_ok) => {
            if !health_ok {
                warn!("⚠️  Some health checks reported issues, but continuing server startup...");
            } else {
                info!("✅ All systems operational!");
            }
        }
        Err(_) => {
            warn!("⏰ Health check timed out after 30 seconds, continuing server startup...");
        }
    }

    let _monitor_handle = monitor_registry.clone().start_background_monitoring().await;

    start_tcp_server(app_config.server).await
}

/// Запуск основного TCP сервера
async fn start_tcp_server(server_config: ServerConfig) -> Result<()> {
    let addr = server_config.get_addr();

    let listener = TcpListener::bind(&addr).await?;
    info!(target: "server", "🚀 Server listening on {}", addr);

    let connection_manager = Arc::new(ConnectionManager::new());
    let session_manager = Arc::new(SessionManager::new(connection_manager.clone()));
    let packet_service = PacketService::new(Arc::clone(&session_manager));
    let dispatcher = Arc::new(Dispatcher::spawn(20, packet_service));

    session_manager.start_heartbeat().await;

    let heartbeat_sender = Arc::new(HeartbeatSender::new(Arc::clone(&session_manager)));
    heartbeat_sender.start().await;

    info!("💓 Heartbeat system started");
    info!("🎯 Server is ready and accepting connections");

    loop {
        let (stream, _) = listener.accept().await?;
        let peer = stream.peer_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());

        let dispatcher = dispatcher.clone();
        let connection_manager = connection_manager.clone();
        let session_manager = session_manager.clone();

        tokio::spawn(async move {
            match handle_connection(stream, peer, dispatcher, session_manager, connection_manager).await {
                Ok(()) => {
                    info!(target: "server", "Connection with {} closed cleanly", peer);
                }
                Err(e) => {
                    error!(target: "server", "Connection {} error: {}", peer, e);
                }
            }
        });
    }
}

async fn initialize_database(db_config: ConfigDatabaseConfig) {
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