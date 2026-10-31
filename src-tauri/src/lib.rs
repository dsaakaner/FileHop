pub mod server;

use tauri::{Manager, command, Emitter};
use server::state::AppState;
use server::models::{DeviceInfo, AppSettings, PendingDownload};
use std::net::{UdpSocket, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};

#[command]
fn save_incoming_file(temp_path: String, dest_path: String) -> Result<String, String> {
    match std::fs::copy(&temp_path, &dest_path) {
        Ok(_) => { let _ = std::fs::remove_file(&temp_path); Ok("Файл успешно сохранен".to_string()) }
        Err(e) => Err(format!("Ошибка сохранения: {}", e))
    }
}

#[command]
async fn prepare_file_for_download(file_path: String, state: tauri::State<'_, AppState>) -> Result<String, String> {
    let token = format!("{:016x}", rand::random::<u64>());
    
    let path = std::path::Path::new(&file_path);
    let filename = path.file_name().unwrap_or_else(|| std::ffi::OsStr::new("unknown")).to_string_lossy().to_string();
    let size = std::fs::metadata(&file_path).map_err(|e| format!("Не удалось прочитать файл: {}", e))?.len() as usize;

    let mut temp_path = state.temp_dir.clone();
    temp_path.push(format!("dl_{}_{}", token, filename));

    std::fs::copy(&file_path, &temp_path).map_err(|e| format!("Ошибка копирования: {}", e))?;

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let download = PendingDownload {
        token: token.clone(),
        filename,
        temp_path: temp_path.to_string_lossy().to_string(),
        size,
        created_at: now,
    };

    let mut downloads = state.pending_downloads.write().await;
    downloads.push(download);
    downloads.retain(|d| now - d.created_at < 600);

    Ok(token)
}

#[command]
async fn send_file_to_device(
    target_ip: String,
    file_path: String,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    
    let mut multipart = reqwest::multipart::Form::new();
    let file_bytes = tokio::fs::read(&file_path).await.map_err(|e| e.to_string())?;
    let file_part = reqwest::multipart::Part::bytes(file_bytes)
        .file_name(std::path::Path::new(&file_path).file_name().unwrap().to_string_lossy().to_string());
    multipart = multipart.part("file", file_part);
    
    let url = format!("http://{}:7431/upload_pc", target_ip);
    
    let response = client.post(&url)
        .multipart(multipart)
        .send()
        .await
        .map_err(|e| format!("Ошибка сети: {}", e))?;
    
    if response.status().is_success() {
        let _ = app.emit("transfer-complete", serde_json::json!({
            "status": "success",
            "message": "Файл успешно отправлен"
        }));
        Ok(())
    } else {
        let error_text = response.text().await.unwrap_or_default();
        let _ = app.emit("transfer-complete", serde_json::json!({
            "status": "error",
            "message": error_text
        }));
        Err(format!("Ошибка сервера: {}", error_text))
    }
}

#[command]
fn get_devices(state: tauri::State<AppState>) -> Vec<DeviceInfo> { 
    state.devices.blocking_read().clone() 
}

#[command]
fn update_settings(theme: String, manual_ip: String, state: tauri::State<AppState>) {
    let mut settings = state.settings.blocking_write();
    settings.theme = theme; 
    settings.manual_ip = manual_ip;
}

#[command]
fn get_settings(state: tauri::State<AppState>) -> AppSettings { 
    state.settings.blocking_read().clone() 
}

fn start_udp_discovery(state: AppState) {
    std::thread::spawn(move || {
        let socket = UdpSocket::bind("0.0.0.0:7432").unwrap();
        socket.set_broadcast(true).unwrap();
        let broadcast_addr: SocketAddr = "255.255.255.255:7432".parse().unwrap();
        
        let my_ip = server::state::find_local_ip();
        let hostname = hostname::get().unwrap_or_else(|_| std::ffi::OsString::from("Unknown-PC")).to_string_lossy().to_string();

        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = socket.send_to(format!("BEACON|{}|{}", hostname, my_ip).as_bytes(), &broadcast_addr);
        }
    });

    let state_receiver = state.clone();
    std::thread::spawn(move || {
        let socket = UdpSocket::bind("0.0.0.0:7433").unwrap();
        let mut buf = [0; 1024];
        loop {
            if let Ok((len, _addr)) = socket.recv_from(&mut buf) {
                let msg = String::from_utf8_lossy(&buf[..len]);
                if msg.starts_with("BEACON|") {
                    let parts: Vec<&str> = msg.trim().split('|').collect();
                    if parts.len() == 3 {
                        let name = parts[1].to_string();
                        let ip = parts[2].to_string();
                        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                        let mut devices = state_receiver.devices.blocking_write();
                        if let Some(pos) = devices.iter().position(|d| d.ip == ip) { devices[pos].last_seen = now; }
                        else { devices.push(DeviceInfo { name, ip, last_seen: now }); }
                        devices.retain(|d| now - d.last_seen < 10);
                    }
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            save_incoming_file, 
            prepare_file_for_download,
            send_file_to_device,
            get_devices, 
            update_settings, 
            get_settings
        ])
        .setup(|app| {
            let state = AppState::new(app.handle().clone());
            app.manage(state.clone());
            start_udp_discovery(state.clone());

            let state_clone = state.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(async {
                    let ip = state_clone.get_active_ip().await;
                    let router = server::create_router(state_clone);
                    let listener = tokio::net::TcpListener::bind("0.0.0.0:7431").await.unwrap();
                    println!("[Server] FileHop HTTP running on http://{}:7431", ip);
                    axum::serve(listener, router).await.unwrap();
                });
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
