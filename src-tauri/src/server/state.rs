use crate::server::models::{AppSettings, DeviceInfo, FileReceivedPayload, PendingDownload};
use rand::Rng;
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use tokio::sync::{Mutex, RwLock, oneshot};

pub const DOWNLOAD_TOKEN_TTL_SECS: u64 = 600;
pub const PC_UPLOAD_CONFIRM_TIMEOUT_SECS: u64 = 120;
pub const MAX_PC_UPLOAD_SIZE: usize = 4 * 1024 * 1024 * 1024;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug)]
pub struct PendingIncomingDecision {
    pub sender: oneshot::Sender<bool>,
}

#[derive(Clone, Debug)]
pub struct IncomingQueueItem {
    pub payload: FileReceivedPayload,
}

fn is_virtual_interface(name: &str) -> bool {
    let n = name.to_lowercase();
    [
        "virtual",
        "vethernet",
        "vmware",
        "virtualbox",
        "vbox",
        "docker",
        "br-",
        "veth",
        "wg",
        "tailscale",
        "tun",
        "tap",
        "wsl",
        "hyper-v",
        "loopback",
        "utun",
        "zt",
    ]
    .iter()
    .any(|m| n.contains(m))
}

pub fn list_local_ipv4s() -> Vec<String> {
    let mut ips = Vec::new();
    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        for (name, ip) in interfaces {
            if is_virtual_interface(&name) {
                continue;
            }
            if let IpAddr::V4(v4) = ip {
                if v4.is_loopback() || v4.is_link_local() {
                    continue;
                }
                let s = v4.to_string();
                if !ips.contains(&s) {
                    ips.push(s);
                }
            }
        }
    }
    ips
}

pub fn find_local_ip() -> String {
    match local_ip_address::list_afinet_netifas() {
        Ok(interfaces) => {
            let mut fallback_ip = String::new();
            for (name, ip) in interfaces {
                if is_virtual_interface(&name) {
                    continue;
                }
                let ip_str = ip.to_string();
                if ip_str.starts_with("127.") || ip_str.contains(':') {
                    continue;
                }

                if ip_str.starts_with("192.168.") || ip_str.starts_with("10.") {
                    println!("[Network] Active IP: {} ({})", ip_str, name);
                    return ip_str;
                }
                if fallback_ip.is_empty() {
                    fallback_ip = ip_str;
                }
            }
            if !fallback_ip.is_empty() {
                return fallback_ip;
            }
            "127.0.0.1".to_string()
        }
        Err(_) => "127.0.0.1".to_string(),
    }
}

#[derive(Clone)]
pub struct AppState {
    pub app_handle: Option<AppHandle>,
    pub access_code: Arc<RwLock<String>>,
    pub temp_dir: PathBuf,
    pub devices: Arc<RwLock<Vec<DeviceInfo>>>,
    pub settings: Arc<RwLock<AppSettings>>,
    pub pending_downloads: Arc<RwLock<Vec<PendingDownload>>>,
    pub failed_web_attempts: Arc<RwLock<HashMap<String, (u32, u64)>>>,
    pub server_status: Arc<RwLock<ServerStatus>>,
    pub incoming_queue: Arc<Mutex<VecDeque<IncomingQueueItem>>>,
    pub pc_decisions: Arc<Mutex<HashMap<String, PendingIncomingDecision>>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ServerStatus {
    pub http_ok: bool,
    pub http_error: Option<String>,
    pub discovery_sender_ok: bool,
    pub discovery_sender_error: Option<String>,
    pub discovery_receiver_ok: bool,
    pub discovery_receiver_error: Option<String>,
}

impl AppState {
    pub fn new(app_handle: AppHandle) -> Self {
        let code = format!("{:06}", rand::thread_rng().gen_range(1..1000000));
        let mut dir = std::env::temp_dir();
        dir.push("FileHop_temp");
        let _ = std::fs::create_dir_all(&dir);

        let mut state = Self {
            app_handle: Some(app_handle),
            access_code: Arc::new(RwLock::new(code)),
            temp_dir: dir,
            devices: Arc::new(RwLock::new(Vec::new())),
            settings: Arc::new(RwLock::new(AppSettings::default())),
            pending_downloads: Arc::new(RwLock::new(Vec::new())),
            failed_web_attempts: Arc::new(RwLock::new(HashMap::new())),
            server_status: Arc::new(RwLock::new(ServerStatus::default())),
            incoming_queue: Arc::new(Mutex::new(VecDeque::new())),
            pc_decisions: Arc::new(Mutex::new(HashMap::new())),
        };
        state.load_settings_from_disk();
        state.cleanup_temp_orphans_on_startup();
        state
    }

    pub fn new_headless(temp_dir: PathBuf) -> Self {
        let code = format!("{:06}", rand::thread_rng().gen_range(1..1000000));
        let _ = std::fs::create_dir_all(&temp_dir);
        Self {
            app_handle: None,
            access_code: Arc::new(RwLock::new(code)),
            temp_dir,
            devices: Arc::new(RwLock::new(Vec::new())),
            settings: Arc::new(RwLock::new(AppSettings::default())),
            pending_downloads: Arc::new(RwLock::new(Vec::new())),
            failed_web_attempts: Arc::new(RwLock::new(HashMap::new())),
            server_status: Arc::new(RwLock::new(ServerStatus::default())),
            incoming_queue: Arc::new(Mutex::new(VecDeque::new())),
            pc_decisions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn settings_path(&self) -> Option<PathBuf> {
        self.app_handle
            .as_ref()
            .and_then(|h| h.path().app_config_dir().ok())
            .map(|d| d.join("settings.json"))
    }

    fn load_settings_from_disk(&mut self) {
        let Some(path) = self.settings_path() else {
            return;
        };
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(parsed) = serde_json::from_slice::<AppSettings>(&bytes)
        {
            let settings = AppSettings {
                theme: match parsed.theme.as_str() {
                    "light" | "dark" | "system" => parsed.theme,
                    _ => "system".to_string(),
                },
                manual_ip: parsed.manual_ip.trim().to_string(),
            };
            if let Ok(mut s) = self.settings.try_write() {
                *s = settings;
            } else {
                // best effort in ctor
                let rt = tokio::runtime::Handle::try_current();
                if let Ok(h) = rt {
                    let settings_lock = self.settings.clone();
                    let settings_clone = settings;
                    h.spawn(async move {
                        *settings_lock.write().await = settings_clone;
                    });
                }
            }
        }
    }

    pub async fn save_settings_to_disk(&self) -> Result<(), String> {
        let Some(path) = self.settings_path() else {
            return Err("Не удалось определить каталог настроек".to_string());
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Не удалось создать каталог настроек: {}", e))?;
        }
        let settings = self.settings.read().await.clone();
        let json = serde_json::to_vec_pretty(&settings)
            .map_err(|e| format!("Не удалось сериализовать настройки: {}", e))?;
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json)
            .await
            .map_err(|e| format!("Не удалось записать настройки: {}", e))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| format!("Не удалось сохранить настройки: {}", e))?;
        Ok(())
    }

    pub fn cleanup_temp_orphans_on_startup(&self) {
        let dir = self.temp_dir.clone();
        let _ = std::fs::create_dir_all(&dir);
        let now = now_secs();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let is_managed = name.starts_with("up_")
                    || name.starts_with("pc_")
                    || name.starts_with("dl_")
                    || name.starts_with("pcstage_");
                if !is_managed {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(now);
                if now.saturating_sub(modified) > 300 {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    pub fn is_path_in_temp_dir(&self, path: &Path) -> bool {
        let Ok(temp_canon) = self.temp_dir.canonicalize() else {
            return false;
        };
        let Ok(path_canon) = path.canonicalize() else {
            return false;
        };
        path_canon.starts_with(temp_canon)
    }

    pub fn get_temp_dir(&self) -> PathBuf {
        self.temp_dir.clone()
    }

    pub async fn get_active_ip(&self) -> String {
        let settings = self.settings.read().await;
        if !settings.manual_ip.is_empty() {
            settings.manual_ip.clone()
        } else {
            find_local_ip()
        }
    }

    pub async fn add_pending_download(
        &self,
        token: String,
        filename: String,
        source_path: String,
        size: u64,
        managed: bool,
    ) {
        let now = now_secs();
        let mut downloads = self.pending_downloads.write().await;
        downloads.push(PendingDownload {
            token,
            filename,
            source_path,
            size,
            created_at: now,
            managed,
        });
        drop(downloads);
        self.cleanup_expired_downloads().await;
    }

    pub async fn get_pending_download(&self, token: &str) -> Option<PendingDownload> {
        let now = now_secs();
        let mut downloads = self.pending_downloads.write().await;
        let found = downloads.iter().find(|d| d.token == token).cloned();
        if let Some(ref item) = found
            && now.saturating_sub(item.created_at) >= DOWNLOAD_TOKEN_TTL_SECS
        {
            downloads.retain(|d| d.token != token);
            let managed = item.managed;
            let path = item.source_path.clone();
            drop(downloads);
            if managed {
                let _ = tokio::fs::remove_file(&path).await;
            }
            return None;
        }
        found
    }

    pub async fn cleanup_expired_downloads(&self) {
        let now = now_secs();
        let mut downloads = self.pending_downloads.write().await;
        let mut expired = Vec::new();
        downloads.retain(|d| {
            if now.saturating_sub(d.created_at) >= DOWNLOAD_TOKEN_TTL_SECS {
                expired.push(d.clone());
                false
            } else {
                true
            }
        });
        drop(downloads);
        for item in expired {
            if item.managed {
                let _ = tokio::fs::remove_file(&item.source_path).await;
            }
        }
    }

    pub async fn cleanup_rate_limits(&self) {
        let now = now_secs();
        let mut attempts = self.failed_web_attempts.write().await;
        attempts.retain(|_, (_, ts)| now.saturating_sub(*ts) < 300);
    }

    pub async fn check_rate_limit(&self, ip: &str) -> bool {
        let now = now_secs();
        let mut attempts = self.failed_web_attempts.write().await;
        attempts.retain(|_, (_, ts)| now.saturating_sub(*ts) < 300);
        let entry = attempts.entry(ip.to_string()).or_insert((0, now));
        if now.saturating_sub(entry.1) >= 60 {
            entry.0 = 0;
            entry.1 = now;
        }
        entry.0 += 1;
        entry.0 <= 5
    }

    pub async fn rotate_access_code(&self) -> String {
        let code = format!("{:06}", rand::thread_rng().gen_range(1..1000000));
        *self.access_code.write().await = code.clone();
        code
    }

    pub async fn enqueue_incoming(&self, payload: FileReceivedPayload) {
        let mut q = self.incoming_queue.lock().await;
        q.push_back(IncomingQueueItem { payload });
    }

    pub async fn pop_incoming(&self) -> Option<FileReceivedPayload> {
        let mut q = self.incoming_queue.lock().await;
        q.pop_front().map(|i| i.payload)
    }

    pub async fn remove_incoming_by_id(&self, id: &str) -> Option<FileReceivedPayload> {
        let mut q = self.incoming_queue.lock().await;
        if let Some(pos) = q.iter().position(|i| i.payload.id == id) {
            return Some(q.remove(pos).unwrap().payload);
        }
        None
    }

    pub async fn register_pc_decision(&self, id: String) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        let mut map = self.pc_decisions.lock().await;
        map.insert(id, PendingIncomingDecision { sender: tx });
        rx
    }

    pub async fn resolve_pc_decision(&self, id: &str, accepted: bool) -> bool {
        let mut map = self.pc_decisions.lock().await;
        if let Some(pending) = map.remove(id) {
            let _ = pending.sender.send(accepted);
            return true;
        }
        false
    }
}
