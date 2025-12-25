pub mod license_monitor;
pub mod server_monitor;
pub mod community_monitor;
pub mod records_monitor; // Добавляем новый модуль

pub use license_monitor::LicenseMonitor;
pub use server_monitor::ServerMonitor;
pub use community_monitor::CommunityMonitor;
pub use records_monitor::RecordsMonitor; // Экспортируем