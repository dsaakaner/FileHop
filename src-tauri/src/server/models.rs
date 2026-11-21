use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct StatusResponse {
    pub status: String,
    pub local_ip: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileReceivedPayload {
    pub id: String,
    pub filename: String,
    pub temp_path: String,
    pub size: usize,
    pub source: String,
    pub sender_ip: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct IncomingTransferRequest {
    pub id: String,
    pub filename: String,
    pub size: usize,
    pub sender_ip: String,
    pub sender_name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub ip: String,
    pub last_seen: u64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AppSettings {
    pub theme: String,
    pub manual_ip: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PendingDownload {
    pub token: String,
    pub filename: String,
    pub source_path: String,
    pub size: u64,
    pub created_at: u64,
    pub managed: bool,
}
