use std::sync::Arc;
use std::path::PathBuf;
use tokio::sync::RwLock;
use rand::Rng;
use tauri::AppHandle;
use crate::server::models::{DeviceInfo, AppSettings, PendingDownload};
use std::time::{SystemTime, UNIX_EPOCH};
use std::collections::HashMap;

pub fn find_local_ip() -> String {
    match local_ip_address::list_afinet_netifas() {
        Ok(interfaces) => {
            let mut fallback_ip = String::new();
            for (name, ip) in interfaces {
                let ip_str = ip.to_string();
                if ip_str.starts_with("127.") || ip_str.contains(':') { continue; }
                
                let name_lower = name.to_lowercase();
                if name_lower.contains("virtual") || name_lower.contains("vethernet") || 
                   name_lower.contains("vmware") || name_lower.contains("virtualbox") || 
                   name_lower.contains("tun") || name_lower.contains("tap") || name_lower.contains("wsl") {
                    continue;
                }
                
                if ip_str.starts_with("192.168.") || ip_str.starts_with("10.") {
                    println!("[Network] Active IP: {} ({})", ip_str, name);
                    return ip_str;
                }
                if fallback_ip.is_empty() { fallback_ip = ip_str; }
            }
            if !fallback_ip.is_empty() { return fallback_ip; }
            "127.0.0.1".to_string()
        }
        Err(_) => "127.0.0.1".to_string(),
    }
}

#[derive(Clone)]
pub struct AppState {
    pub app_handle: AppHandle,
    pub access_code: Arc<RwLock<String>>,
    pub connected_clients: Arc<RwLock<usize>>,
    pub temp_dir: PathBuf,
    pub devices: Arc<RwLock<Vec<DeviceInfo>>>,
    pub settings: Arc<RwLock<AppSettings>>,
    pub pending_downloads: Arc<RwLock<Vec<PendingDownload>>>,
    pub failed_web_attempts: Arc<RwLock<HashMap<String, (u32, u64)>>>, // IP -> (count, timestamp)
}

impl AppState {
    pub fn new(app_handle: AppHandle) -> Self {
        let code = format!("{:06}", rand::thread_rng().gen_range(1..1000000));
        let mut dir = std::env::temp_dir();
        dir.push("FileHop_temp");
        let _ = std::fs::create_dir_all(&dir);

        Self {
            app_handle,
            access_code: Arc::new(RwLock::new(code)),
            connected_clients: Arc::new(RwLock::new(0)),
            temp_dir: dir,
            devices: Arc::new(RwLock::new(Vec::new())),
            settings: Arc::new(RwLock::new(AppSettings {
                theme: "system".to_string(),
                manual_ip: String::new(),
            })),
            pending_downloads: Arc::new(RwLock::new(Vec::new())),
            failed_web_attempts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get_active_ip(&self) -> String {
        let settings = self.settings.read().await;
        if !settings.manual_ip.is_empty() {
            settings.manual_ip.clone()
        } else {
            find_local_ip()
        }
    }

    pub async fn add_pending_download(&self, token: String, filename: String, temp_path: String, size: usize) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mut downloads = self.pending_downloads.write().await;
        downloads.push(PendingDownload { token, filename, temp_path, size, created_at: now });
        downloads.retain(|d| now - d.created_at < 600);
    }

    pub async fn get_pending_download(&self, token: &str) -> Option<PendingDownload> {
        let downloads = self.pending_downloads.read().await;
        downloads.iter().find(|d| d.token == token).cloned()
    }

    pub async fn check_rate_limit(&self, ip: &str) -> bool {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mut attempts = self.failed_web_attempts.write().await;
        let entry = attempts.entry(ip.to_string()).or_insert((0, now));
        
        if now - entry.1 > 60 {
            entry.0 = 0;
            entry.1 = now;
        }
        
        entry.0 += 1;
        entry.0 <= 5 // Разрешаем максимум 5 попыток в минуту
    }
}
