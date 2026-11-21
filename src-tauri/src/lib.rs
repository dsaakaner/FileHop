pub mod server;

use server::filename::sanitize_filename_component;
use server::models::{AppSettings, DeviceInfo, FileReceivedPayload};
use server::state::{AppState, ServerStatus, now_secs};
use std::ffi::OsStr;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{Emitter, Manager, command};

#[command]
async fn get_incoming_queue(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FileReceivedPayload>, String> {
    let q = state.incoming_queue.lock().await;
    Ok(q.iter().map(|i| i.payload.clone()).collect())
}

#[command]
async fn save_incoming_file(
    id: String,
    dest_path: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let payload = state
        .remove_incoming_by_id(&id)
        .await
        .ok_or_else(|| "Входящий файл не найден или уже обработан".to_string())?;
    let temp = PathBuf::from(&payload.temp_path);
    if !state.is_path_in_temp_dir(&temp) {
        state.enqueue_incoming(payload).await;
        return Err("Недопустимый путь временного файла".to_string());
    }
    tokio::fs::copy(&temp, &dest_path)
        .await
        .map_err(|e| format!("Ошибка сохранения: {}", e))?;
    let _ = tokio::fs::remove_file(&temp).await;
    if let Some(h) = &state.app_handle {
        let _ = h.emit("incoming-queue-updated", ());
    }
    Ok("Файл успешно сохранен".to_string())
}

#[command]
async fn discard_incoming_file(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if let Some(payload) = state.remove_incoming_by_id(&id).await {
        let temp = PathBuf::from(&payload.temp_path);
        if state.is_path_in_temp_dir(&temp) {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        if let Some(h) = &state.app_handle {
            let _ = h.emit("incoming-queue-updated", ());
        }
    }
    Ok(())
}

#[command]
async fn prepare_file_for_download(
    file_path: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let token = format!("{:016x}", rand::random::<u64>());

    let path = Path::new(&file_path);
    let meta = tokio::fs::metadata(&file_path)
        .await
        .map_err(|e| format!("Не удалось прочитать файл: {}", e))?;
    if !meta.is_file() {
        return Err("Можно отправлять только обычные файлы".to_string());
    }
    let filename = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("unknown"))
        .to_string_lossy()
        .to_string();
    let filename = sanitize_filename_component(&filename);
    let size = meta.len();

    state
        .add_pending_download(token.clone(), filename, file_path, size, false)
        .await;
    Ok(token)
}

use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicBool, Ordering};

static SEND_CANCEL: std::sync::OnceLock<StdArc<AtomicBool>> = std::sync::OnceLock::new();

fn send_cancel_flag() -> StdArc<AtomicBool> {
    SEND_CANCEL
        .get_or_init(|| StdArc::new(AtomicBool::new(false)))
        .clone()
}

#[command]
async fn cancel_send_file() -> Result<(), String> {
    send_cancel_flag().store(true, Ordering::SeqCst);
    Ok(())
}

#[command]
async fn respond_pc_transfer(
    id: String,
    accepted: bool,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    Ok(state.resolve_pc_decision(&id, accepted).await)
}

#[command]
async fn rotate_access_code(state: tauri::State<'_, AppState>) -> Result<String, String> {
    Ok(state.rotate_access_code().await)
}

#[command]
async fn send_file_to_device(
    target_ip: String,
    file_path: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    use tokio::io::AsyncReadExt;

    let ip: IpAddr = target_ip
        .trim()
        .parse()
        .map_err(|_| "Некорректный IP-адрес устройства".to_string())?;

    let filename = sanitize_filename_component(
        &Path::new(&file_path)
            .file_name()
            .unwrap_or_else(|| OsStr::new("unknown"))
            .to_string_lossy(),
    );
    let total = tokio::fs::metadata(&file_path)
        .await
        .map_err(|e| format!("Ошибка чтения файла: {}", e))?
        .len();
    let sender_name = hostname::get()
        .map(|h| sanitize_filename_component(&h.to_string_lossy()))
        .unwrap_or_else(|_| "FileHop-PC".to_string());

    let cancel = send_cancel_flag();
    cancel.store(false, Ordering::SeqCst);

    let boundary = format!("----FileHopBoundary{:016x}", rand::random::<u64>());
    let header = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    );
    let footer = format!("\r\n--{boundary}--\r\n");

    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Vec<u8>>>(8);
    let progress_app = app.clone();
    let source = file_path.clone();
    let cancel_task = cancel.clone();
    tokio::spawn(async move {
        if tx.send(Ok(header.into_bytes())).await.is_err() {
            return;
        }
        let mut file = match tokio::fs::File::open(&source).await {
            Ok(file) => file,
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                return;
            }
        };
        let mut buf = vec![0u8; 256 * 1024];
        let mut sent = 0u64;
        let mut last_emit = std::time::Instant::now();
        let _ = progress_app.emit(
            "transfer-progress",
            serde_json::json!({"percent": 0u32, "sent": 0u64, "total": total}),
        );
        loop {
            if cancel_task.load(Ordering::SeqCst) {
                let _ = tx
                    .send(Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "cancelled",
                    )))
                    .await;
                return;
            }
            match file.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    sent += n as u64;
                    let percent = sent.saturating_mul(100).checked_div(total).unwrap_or(0) as u32;
                    if last_emit.elapsed() >= Duration::from_millis(150) || sent == total {
                        last_emit = std::time::Instant::now();
                        let _ = progress_app.emit(
                            "transfer-progress",
                            serde_json::json!({"percent": percent, "sent": sent, "total": total}),
                        );
                    }
                    if tx.send(Ok(buf[..n].to_vec())).await.is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            }
        }
        let _ = tx.send(Ok(footer.into_bytes())).await;
    });

    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let body = reqwest::Body::wrap_stream(stream);
    let url = match ip {
        IpAddr::V4(v4) => format!("http://{}:7431/upload_pc", v4),
        IpAddr::V6(v6) => format!("http://[{}]:7431/upload_pc", v6),
    };

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60 * 60))
        .build()
        .map_err(|e| format!("Ошибка клиента: {}", e))?;

    let response = client
        .post(&url)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={}", boundary),
        )
        .header("X-FileHop-PC-Upload", "2")
        .header("X-FileHop-Sender-Name", sender_name)
        .body(body)
        .send()
        .await
        .map_err(|e| {
            let msg = if cancel.load(Ordering::SeqCst) {
                "Передача отменена пользователем".to_string()
            } else if e.is_timeout() {
                "Таймаут передачи. Проверьте соединение и повторите попытку.".to_string()
            } else if e.is_connect() {
                "Не удалось подключиться к устройству. Проверьте, запущен ли FileHop на получателе."
                    .to_string()
            } else {
                format!("Ошибка сети: {}", e)
            };
            let _ = app.emit(
                "transfer-complete",
                serde_json::json!({"status": "error", "message": msg}),
            );
            msg
        })?;

    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();
    if status.is_success() {
        let _ = app.emit(
            "transfer-progress",
            serde_json::json!({"percent": 100u32, "sent": total, "total": total}),
        );
        let _ = app.emit(
            "transfer-complete",
            serde_json::json!({"status": "success", "message": "Файл успешно отправлен!"}),
        );
        Ok(())
    } else {
        let msg = if body_text.is_empty() {
            format!("Ошибка сервера: {}", status)
        } else {
            body_text
        };
        let _ = app.emit(
            "transfer-complete",
            serde_json::json!({"status": "error", "message": msg}),
        );
        Err(msg)
    }
}

#[command]
async fn get_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceInfo>, String> {
    let now = now_secs();
    let mut devices = state.devices.write().await;
    devices.retain(|d| now.saturating_sub(d.last_seen) < 10);
    Ok(devices.clone())
}

fn validate_manual_ip(ip: &str) -> Result<String, String> {
    let trimmed = ip.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let parsed: IpAddr = trimmed.parse().map_err(|_| {
        "Некорректный IP-адрес. Используйте IPv4, например 192.168.1.10".to_string()
    })?;
    match parsed {
        IpAddr::V4(v4) => Ok(v4.to_string()),
        IpAddr::V6(_) => Err("Поддерживается только IPv4-адрес".to_string()),
    }
}

#[command]
async fn update_settings(
    theme: String,
    manual_ip: String,
    state: tauri::State<'_, AppState>,
) -> Result<AppSettings, String> {
    let normalized_ip = validate_manual_ip(&manual_ip)?;
    let normalized_theme = match theme.as_str() {
        "light" | "dark" | "system" => theme,
        _ => "system".to_string(),
    };
    {
        let mut settings = state.settings.write().await;
        settings.theme = normalized_theme;
        settings.manual_ip = normalized_ip;
    }
    state.save_settings_to_disk().await?;
    Ok(state.settings.read().await.clone())
}

#[command]
async fn get_settings(state: tauri::State<'_, AppState>) -> Result<AppSettings, String> {
    Ok(state.settings.read().await.clone())
}

#[command]
async fn get_server_status(state: tauri::State<'_, AppState>) -> Result<ServerStatus, String> {
    Ok(state.server_status.read().await.clone())
}

#[command]
async fn get_access_code(state: tauri::State<'_, AppState>) -> Result<String, String> {
    Ok(state.access_code.read().await.clone())
}

#[command]
async fn get_active_ip(state: tauri::State<'_, AppState>) -> Result<String, String> {
    Ok(state.get_active_ip().await)
}

fn sanitize_device_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cleaned: String = trimmed.chars().take(64).collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn start_background(state: AppState) {
    // HTTP server
    let http_state = state.clone();
    tauri::async_runtime::spawn(async move {
        let ip = http_state.get_active_ip().await;
        let router = server::create_router(http_state.clone());
        match tokio::net::TcpListener::bind("0.0.0.0:7431").await {
            Ok(listener) => {
                {
                    let mut st = http_state.server_status.write().await;
                    st.http_ok = true;
                    st.http_error = None;
                }
                println!("[Server] FileHop HTTP running on http://{}:7431", ip);

                let cleanup_state = http_state.clone();
                tauri::async_runtime::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(60));
                    loop {
                        interval.tick().await;
                        cleanup_state.cleanup_expired_downloads().await;
                        cleanup_state.cleanup_rate_limits().await;
                    }
                });

                if let Err(e) = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                {
                    let msg = format!("Ошибка HTTP-сервера: {}", e);
                    eprintln!("[Server] {}", msg);
                    let mut st = http_state.server_status.write().await;
                    st.http_ok = false;
                    st.http_error = Some(msg);
                }
            }
            Err(e) => {
                let msg = format!("Не удалось слушать порт 7431: {}", e);
                eprintln!("[Server] {}", msg);
                let mut st = http_state.server_status.write().await;
                st.http_ok = false;
                st.http_error = Some(msg);
            }
        }
    });

    // UDP beacon sender
    let sender_state = state.clone();
    tauri::async_runtime::spawn(async move {
        let socket = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("Не удалось создать UDP-сокет отправки: {}", e);
                eprintln!("[UDP] {}", msg);
                let mut st = sender_state.server_status.write().await;
                st.discovery_sender_ok = false;
                st.discovery_sender_error = Some(msg);
                return;
            }
        };
        if let Err(e) = socket.set_broadcast(true) {
            let msg = format!("Не удалось включить broadcast: {}", e);
            eprintln!("[UDP] {}", msg);
            let mut st = sender_state.server_status.write().await;
            st.discovery_sender_ok = false;
            st.discovery_sender_error = Some(msg);
            return;
        }
        {
            let mut st = sender_state.server_status.write().await;
            st.discovery_sender_ok = true;
            st.discovery_sender_error = None;
        }

        let hostname = hostname::get()
            .unwrap_or_else(|_| std::ffi::OsString::from("Unknown-PC"))
            .to_string_lossy()
            .chars()
            .take(64)
            .collect::<String>();
        let broadcast_addr = "255.255.255.255:7433";

        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let ip = sender_state.get_active_ip().await;
            if ip == "127.0.0.1" {
                continue;
            }
            let msg = format!("BEACON|{}|{}", hostname, ip);
            let _ = socket.send_to(msg.as_bytes(), broadcast_addr).await;
        }
    });

    // UDP beacon receiver
    let recv_state = state.clone();
    tauri::async_runtime::spawn(async move {
        let socket = match tokio::net::UdpSocket::bind("0.0.0.0:7433").await {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("Не удалось слушать порт 7433: {}", e);
                eprintln!("[UDP] {}", msg);
                let mut st = recv_state.server_status.write().await;
                st.discovery_receiver_ok = false;
                st.discovery_receiver_error = Some(msg);
                return;
            }
        };
        {
            let mut st = recv_state.server_status.write().await;
            st.discovery_receiver_ok = true;
            st.discovery_receiver_error = None;
        }

        let mut buf = [0u8; 2048];
        let mut last_update: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        loop {
            let (len, _addr) = match socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            if len == 0 || len > 1024 {
                continue;
            }
            let msg = String::from_utf8_lossy(&buf[..len]);
            if !msg.starts_with("BEACON|") {
                continue;
            }
            let parts: Vec<&str> = msg.trim().split('|').collect();
            if parts.len() != 3 {
                continue;
            }
            let Some(name) = sanitize_device_name(parts[1]) else {
                continue;
            };
            let ip_str = parts[2].trim();
            let Ok(parsed_ip) = ip_str.parse::<IpAddr>() else {
                continue;
            };
            let ip = parsed_ip.to_string();

            // ignore self
            let active_ip = recv_state.get_active_ip().await;
            let mut own_ips = server::state::list_local_ipv4s();
            if !active_ip.is_empty() {
                own_ips.push(active_ip);
            }
            if own_ips.iter().any(|o| o == &ip) {
                continue;
            }

            let now = now_secs();
            if let Some(prev) = last_update.get(&ip)
                && now.saturating_sub(*prev) < 1
            {
                continue;
            }
            last_update.insert(ip.clone(), now);
            last_update.retain(|_, t| now.saturating_sub(*t) < 30);

            let mut devices = recv_state.devices.write().await;
            if let Some(pos) = devices.iter().position(|d| d.ip == ip) {
                devices[pos].last_seen = now;
                devices[pos].name = name;
            } else {
                devices.push(DeviceInfo {
                    name,
                    ip,
                    last_seen: now,
                });
            }
            devices.retain(|d| now.saturating_sub(d.last_seen) < 10);
        }
    });

    // periodic device cleanup (B4)
    let cleanup_devices_state = state;
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            let now = now_secs();
            let mut devices = cleanup_devices_state.devices.write().await;
            devices.retain(|d| now.saturating_sub(d.last_seen) < 10);
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Err(e) = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_incoming_queue,
            save_incoming_file,
            discard_incoming_file,
            prepare_file_for_download,
            send_file_to_device,
            cancel_send_file,
            respond_pc_transfer,
            rotate_access_code,
            get_devices,
            update_settings,
            get_settings,
            get_server_status,
            get_access_code,
            get_active_ip
        ])
        .setup(|app| {
            let state = AppState::new(app.handle().clone());
            app.manage(state.clone());
            start_background(state);
            Ok(())
        })
        .run(tauri::generate_context!())
    {
        eprintln!("[FileHop] Ошибка запуска приложения: {}", e);
    }
}
