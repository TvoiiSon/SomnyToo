use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub congestion_control: CongestionConfig,
    pub rate_limiting: RateLimitingConfig,
    pub reputation: ReputationConfig,
    pub auditing: AuditingConfig,
    pub test_mode: TestModeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CongestionConfig {
    pub enabled: bool,
    pub learning_mode: bool,
    pub max_decision_time_ms: u64,
    pub auto_update_interval_sec: u64,
    pub anomaly_threshold_soft: f64,
    pub anomaly_threshold_hard: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitingConfig {
    pub max_packets_per_second: u64,
    pub max_bytes_per_second: u64,
    pub max_connections_per_minute: u64,
    pub burst_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationConfig {
    pub auto_blacklist_enabled: bool,
    pub max_offenses_per_ip: usize,
    pub offense_window_minutes: u64,
    pub cleanup_interval_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditingConfig {
    pub enabled: bool,
    pub max_events: usize,
    pub retention_days: u32,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            congestion_control: CongestionConfig::default(),
            rate_limiting: RateLimitingConfig::default(),
            reputation: ReputationConfig::default(),
            auditing: AuditingConfig::default(),
            test_mode: TestModeConfig::default(),
        }
    }
}

impl Default for CongestionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            learning_mode: false,
            max_decision_time_ms: 10,
            auto_update_interval_sec: 5,
            anomaly_threshold_soft: 0.3,
            anomaly_threshold_hard: 0.7,
        }
    }
}

impl Default for RateLimitingConfig {
    fn default() -> Self {
        Self {
            max_packets_per_second: 1000,
            max_bytes_per_second: 1024 * 1024, // 1 MB/s
            max_connections_per_minute: 10,
            burst_multiplier: 1.5,
        }
    }
}

impl Default for ReputationConfig {
    fn default() -> Self {
        Self {
            auto_blacklist_enabled: true,
            max_offenses_per_ip: 100,
            offense_window_minutes: 60,
            cleanup_interval_hours: 1,
        }
    }
}

impl Default for AuditingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_events: 10_000,
            retention_days: 30,
        }
    }
}

// Безопасный менеджер конфигурации
pub struct ConfigManager {
    config: RwLock<SecurityConfig>,
    config_path: Option<String>,
}

impl ConfigManager {
    pub fn new() -> Self {
        Self {
            config: RwLock::new(SecurityConfig::default()),
            config_path: None,
        }
    }

    pub fn with_path(config_path: String) -> Self {
        Self {
            config: RwLock::new(SecurityConfig::default()),
            config_path: Some(config_path),
        }
    }

    pub async fn load_config(&self) -> Result<(), String> {
        if let Some(path) = &self.config_path {
            let content = tokio::fs::read_to_string(path).await
                .map_err(|e| format!("Failed to read config file: {}", e))?;

            let config: SecurityConfig = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse config: {}", e))?;

            *self.config.write().await = config;
        }
        Ok(())
    }

    pub async fn save_config(&self) -> Result<(), String> {
        if let Some(path) = &self.config_path {
            let config = self.config.read().await;
            let content = serde_json::to_string_pretty(&*config)
                .map_err(|e| format!("Failed to serialize config: {}", e))?;

            tokio::fs::write(path, content).await
                .map_err(|e| format!("Failed to write config file: {}", e))?;
        }
        Ok(())
    }

    pub async fn get_config(&self) -> SecurityConfig {
        self.config.read().await.clone()
    }

    pub async fn update_config<F>(&self, update_fn: F) -> Result<(), String>
    where
        F: FnOnce(&mut SecurityConfig),
    {
        let mut config = self.config.write().await;
        update_fn(&mut config);

        // Валидация конфигурации
        self.validate_config(&config)?;

        // Автосохранение при изменении
        self.save_config().await?;

        Ok(())
    }

    fn validate_config(&self, config: &SecurityConfig) -> Result<(), String> {
        // Валидация значений конфигурации
        if config.congestion_control.anomaly_threshold_soft >= config.congestion_control.anomaly_threshold_hard {
            return Err("anomaly_threshold_soft must be less than anomaly_threshold_hard".to_string());
        }

        if config.congestion_control.anomaly_threshold_soft < 0.0 || config.congestion_control.anomaly_threshold_soft > 1.0 {
            return Err("anomaly_threshold_soft must be between 0.0 and 1.0".to_string());
        }

        if config.rate_limiting.max_packets_per_second == 0 {
            return Err("max_packets_per_second must be greater than 0".to_string());
        }

        Ok(())
    }

    // Геттеры для конкретных разделов конфигурации
    pub async fn get_congestion_config(&self) -> CongestionConfig {
        self.config.read().await.congestion_control.clone()
    }

    pub async fn get_rate_limiting_config(&self) -> RateLimitingConfig {
        self.config.read().await.rate_limiting.clone()
    }

    pub async fn get_reputation_config(&self) -> ReputationConfig {
        self.config.read().await.reputation.clone()
    }

    pub async fn enable_test_mode(&self) -> Result<(), String> {
        self.update_config(|config| {
            config.test_mode.enabled = true;
            config.test_mode.allow_private_ips = true;
            config.test_mode.relaxed_validation = true;
            config.congestion_control.learning_mode = true;
        }).await
    }

    pub async fn disable_test_mode(&self) -> Result<(), String> {
        self.update_config(|config| {
            config.test_mode.enabled = false;
            config.test_mode.allow_private_ips = false;
            config.test_mode.relaxed_validation = false;
        }).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestModeConfig {
    pub enabled: bool,
    pub allow_private_ips: bool,
    pub relaxed_validation: bool,
}

impl Default for TestModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_private_ips: false,
            relaxed_validation: false,
        }
    }
}