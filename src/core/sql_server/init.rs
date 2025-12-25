use super::config::{DatabaseConfig, SecurityConfig};
use super::server::SqlServer;
use super::executor::{QUERY_EXECUTOR};

pub async fn initialize_sql_server() -> Result<(), Box<dyn std::error::Error>> {
    // Создаем конфигурацию
    let db_config = DatabaseConfig::default();
    let security_config = SecurityConfig::default();

    println!("[SQL Server] Initializing with config: {:?}", db_config);

    // Создаем и запускаем сервер
    let mut server = SqlServer::new(db_config, security_config).await?;

    // ✅ ИСПРАВЛЕНО: get_metrics() возвращает ServerMetrics напрямую, а не Result
    let server_metrics = server.get_metrics().await;
    println!("[SQL Server] Metrics system initialized: {:?}", server_metrics);

    server.start().await?;

    // Регистрируем сервер в глобальном executor
    QUERY_EXECUTOR.register_server(server).await;

    // ✅ ИСПРАВЛЕНО: Запускаем фоновые задачи
    start_background_tasks().await;

    println!("[SQL Server] Initialized successfully and ready for queries");
    Ok(())
}

// ✅ ИСПРАВЛЕНО: Убрали глобальный монитор, т.к. PerformanceMonitor не реализует Debug
// Вместо этого используем локальную инициализацию при необходимости

async fn start_background_tasks() {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            perform_maintenance_tasks().await;
        }
    });
}

async fn perform_maintenance_tasks() {
    println!("[Maintenance] Performing periodic maintenance tasks");
}

pub async fn shutdown_sql_server() -> Result<(), Box<dyn std::error::Error>> {
    println!("[SQL Server] Shutting down...");

    QUERY_EXECUTOR.unregister_server().await;

    println!("[SQL Server] Shutdown completed");
    Ok(())
}

pub async fn restart_sql_server() -> Result<(), Box<dyn std::error::Error>> {
    println!("[SQL Server] Restarting...");

    shutdown_sql_server().await?;
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    initialize_sql_server().await?;

    println!("[SQL Server] Restart completed");
    Ok(())
}