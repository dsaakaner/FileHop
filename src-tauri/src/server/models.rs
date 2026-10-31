use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct StatusResponse {
    pub status: String,
    pub code: String,
    pub local_ip: String,
    pub clients_count: usize,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileReceivedPayload {
    pub filename: String,
    pub temp_path: String,
    pub size: usize,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub ip: String,
    pub last_seen: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AppSettings {
    pub theme: String,
    pub manual_ip: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PendingDownload {
    pub token: String,
    pub filename: String,
    pub temp_path: String,
    pub size: usize,
    pub created_at: u64,
}
