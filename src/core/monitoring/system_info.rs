use async_trait::async_trait;
use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
pub struct MemoryInfo {
    pub total: u64,    // в KB
    pub free: u64,     // в KB
    pub avail: u64,    // в KB
}

#[derive(Debug, Serialize, Clone)]
pub struct LoadAvg {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Serialize, Clone)]
pub struct DiskInfo {
    pub total: u64,    // в KB
    pub free: u64,     // в KB
}

#[async_trait]
pub trait SystemInfoProvider: Send + Sync {
    async fn get_memory_info(&self) -> Result<MemoryInfo, String>;
    async fn get_load_avg(&self) -> Result<LoadAvg, String>;
    async fn get_disk_info(&self) -> Result<DiskInfo, String>;
}

// Реальная реализация с sys_info
pub struct RealSystemInfo;

#[async_trait]
impl SystemInfoProvider for RealSystemInfo {
    async fn get_memory_info(&self) -> Result<MemoryInfo, String> {
        tokio::task::spawn_blocking(|| {
            sys_info::mem_info()
                .map(|info| MemoryInfo {
                    total: info.total,
                    free: info.free,
                    avail: info.avail,
                })
                .map_err(|e| e.to_string())
        })
            .await
            .map_err(|e| e.to_string())?
    }

    async fn get_load_avg(&self) -> Result<LoadAvg, String> {
        tokio::task::spawn_blocking(|| {
            sys_info::loadavg()
                .map(|load| LoadAvg {
                    one: load.one,
                    five: load.five,
                    fifteen: load.fifteen,
                })
                .map_err(|e| e.to_string())
        })
            .await
            .map_err(|e| e.to_string())?
    }

    async fn get_disk_info(&self) -> Result<DiskInfo, String> {
        tokio::task::spawn_blocking(|| {
            sys_info::disk_info()
                .map(|disk| DiskInfo {
                    total: disk.total,
                    free: disk.free,
                })
                .map_err(|e| e.to_string())
        })
            .await
            .map_err(|e| e.to_string())?
    }
}

// Mock реализация для тестов
#[cfg(test)]
pub struct MockSystemInfo {
    pub memory_info: Option<MemoryInfo>,
    pub load_avg: Option<LoadAvg>,
    pub disk_info: Option<DiskInfo>,
}

#[cfg(test)]
#[async_trait]
impl SystemInfoProvider for MockSystemInfo {
    async fn get_memory_info(&self) -> Result<MemoryInfo, String> {
        self.memory_info.clone().ok_or_else(|| "Mock memory error".to_string())
    }

    async fn get_load_avg(&self) -> Result<LoadAvg, String> {
        self.load_avg.clone().ok_or_else(|| "Mock loadavg error".to_string())
    }

    async fn get_disk_info(&self) -> Result<DiskInfo, String> {
        self.disk_info.clone().ok_or_else(|| "Mock disk error".to_string())
    }
}