use std::net::IpAddr;
use tokio::sync::RwLock;
use std::collections::{HashSet, HashMap};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct AdminSession {
    pub id: String,
    pub ip: IpAddr,
    pub created_at: Instant,
    pub last_activity: Instant,
    pub permissions: AdminPermissions,
}

#[derive(Debug, Clone)]
pub struct AdminPermissions {
    pub can_ban_ips: bool,
    pub can_whitelist_ips: bool,
    pub can_change_config: bool,
    pub can_view_stats: bool,
}

pub struct AuthManager {
    sessions: RwLock<HashMap<String, AdminSession>>,
    allowed_admin_ips: RwLock<HashSet<IpAddr>>,
    session_timeout: Duration,
}

impl AuthManager {
    pub fn new(allowed_ips: Vec<IpAddr>, session_timeout: Duration) -> Self {
        let mut allowed_set = HashSet::new();
        for ip in allowed_ips {
            allowed_set.insert(ip);
        }

        Self {
            sessions: RwLock::new(HashMap::new()),
            allowed_admin_ips: RwLock::new(allowed_set),
            session_timeout,
        }
    }

    pub async fn authenticate(&self, session_id: &str, ip: IpAddr) -> Result<AdminSession, AuthError> {
        // ИСПРАВЛЕНИЕ: Проверяем разрешенные IP-адреса
        let allowed_ips = self.allowed_admin_ips.read().await;
        if !allowed_ips.contains(&ip) {
            return Err(AuthError::InsufficientPermissions);
        }

        let sessions = self.sessions.read().await;
        let session = sessions.get(session_id)
            .ok_or(AuthError::InvalidSession)?;

        // Проверка IP
        if session.ip != ip {
            return Err(AuthError::IpMismatch);
        }

        // Проверка таймаута
        if Instant::now().duration_since(session.last_activity) > self.session_timeout {
            return Err(AuthError::SessionExpired);
        }

        Ok(session.clone())
    }

    // Добавляем метод для управления разрешенными IP
    pub async fn add_allowed_ip(&self, ip: IpAddr) -> Result<(), AuthError> {
        let mut allowed_ips = self.allowed_admin_ips.write().await;
        allowed_ips.insert(ip);
        Ok(())
    }

    pub async fn remove_allowed_ip(&self, ip: IpAddr) -> Result<(), AuthError> {
        let mut allowed_ips = self.allowed_admin_ips.write().await;
        allowed_ips.remove(&ip);
        Ok(())
    }

    pub async fn get_allowed_ips(&self) -> Vec<IpAddr> {
        let allowed_ips = self.allowed_admin_ips.read().await;
        allowed_ips.iter().cloned().collect()
    }

    pub async fn check_permission(&self, session_id: &str, ip: IpAddr, permission: &str) -> Result<(), AuthError> {
        let session = self.authenticate(session_id, ip).await?;

        match permission {
            "ban_ips" if session.permissions.can_ban_ips => Ok(()),
            "whitelist_ips" if session.permissions.can_whitelist_ips => Ok(()),
            "change_config" if session.permissions.can_change_config => Ok(()),
            "view_stats" if session.permissions.can_view_stats => Ok(()),
            _ => Err(AuthError::InsufficientPermissions),
        }
    }
}

#[derive(Debug)]
pub enum AuthError {
    InvalidSession,
    IpMismatch,
    SessionExpired,
    InsufficientPermissions,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSession => write!(f, "Invalid session"),
            Self::IpMismatch => write!(f, "IP address mismatch"),
            Self::SessionExpired => write!(f, "Session expired"),
            Self::InsufficientPermissions => write!(f, "Insufficient permissions"),
        }
    }
}