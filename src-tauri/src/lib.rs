pub mod server;

use tauri::{Manager, command, Emitter};
use server::state::{AppState, now_secs};
use server::models::{DeviceInfo, AppSettings};
use std::net::{UdpSocket, SocketAddr};
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

#[command]
fn save_incoming_file(temp_path: String, dest_path: String) -> Result<String, String> {
    match std::fs::copy(&temp_path, &dest_path) {
        Ok(_) => {
            let _ = std::fs::remove_file(&temp_path);
            Ok("Файл успешно сохранен".to_string())
        }
        Err(e) => Err(format!("Ошибка сохранения: {}", e)),
    }
}

#[command]
async fn prepare_file_for_download(
    file_path: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let token = format!("{:016x}", rand::random::<u64>());

    let path = Path::new(&file_path);
    let filename = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("unknown"))
        .to_string_lossy()
        .to_string();
    let size = tokio::fs::metadata(&file_path)
        .await
        .map_err(|e| format!("Не удалось прочитать файл: {}", e))?
        .len() as usize;

    let mut temp_path = state.temp_dir.clone();
    temp_path.push(format!("dl_{}_{}", token, filename));

    tokio::fs::copy(&file_path, &temp_path)
        .await
        .map_err(|e| format!("Ошибка копирования: {}", e))?;

    state
        .add_pending_download(
            token.clone(),
            filename,
            temp_path.to_string_lossy().to_string(),
            size,
        )
        .await;

    Ok(token)
}

#[command]
async fn send_file_to_device(
    target_ip: String,
    file_path: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    use tokio::io::AsyncReadExt;

    let filename: String = Path::new(&file_path)
        .file_name()
        .unwrap_or_else(|| OsStr::new("unknown"))
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let total = tokio::fs::metadata(&file_path)
        .await
        .map_err(|e| format!("Ошибка чтения файла: {}", e))?
        .len();

    let boundary = format!("----FileHopBoundary{:016x}", rand::random::<u64>());
    let header = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    );
    let footer = format!("\r\n--{boundary}--\r\n");

    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Vec<u8>>>(4);
    // reqwest принимает futures Stream, а не tokio Receiver.
    let progress_app = app.clone();
    let source = file_path.clone();
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
        loop {
            match file.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    sent += n as u64;
                    let _ = progress_app.emit(
                        "transfer-progress",
                        serde_json::json!({
                            "percent": if total > 0 { (sent * 100 / total) as u32 } else { 0 },
                            "sent": sent,
                            "total": total
                        }),
                    );
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
    let url = format!("http://{}:7431/upload_pc", target_ip);

    let response = reqwest::Client::new()
        .post(&url)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={}", boundary),
        )
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Ошибка сети: {}", e))?;

    if response.status().is_success() {
        let _ = app.emit(
            "transfer-complete",
            serde_json::json!({
                "status": "success",
                "message": "Файл успешно отправлен"
            }),
        );
        Ok(())
    } else {
        let error_text = response.text().await.unwrap_or_default();
        let _ = app.emit(
            "transfer-complete",
            serde_json::json!({
                "status": "error",
                "message": error_text
            }),
        );
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
        let socket = match UdpSocket::bind("0.0.0.0:7432") {
            Ok(socket) => socket,
            Err(e) => {
                eprintln!("[UDP] Не удалось слушать 7432: {}", e);
                return;
            }
        };
        if let Err(e) = socket.set_broadcast(true) {
            eprintln!("[UDP] Не удалось включить broadcast: {}", e);
            return;
        }
        let broadcast_addr = SocketAddr::from(([255, 255, 255, 255], 7432));

        let my_ip = server::state::find_local_ip();
        let hostname = hostname::get()
            .unwrap_or_else(|_| std::ffi::OsString::from("Unknown-PC"))
            .to_string_lossy()
            .to_string();

        loop {
            std::thread::sleep(Duration::from_secs(2));
            let _ = socket.send_to(format!("BEACON|{}|{}", hostname, my_ip).as_bytes(), &broadcast_addr);
        }
    });

    let state_receiver = state.clone();
    std::thread::spawn(move || {
        let socket = match UdpSocket::bind("0.0.0.0:7433") {
            Ok(socket) => socket,
            Err(e) => {
                eprintln!("[UDP] Не удалось слушать 7433: {}", e);
                return;
            }
        };
        let mut buf = [0; 1024];
        loop {
            if let Ok((len, _addr)) = socket.recv_from(&mut buf) {
                let msg = String::from_utf8_lossy(&buf[..len]);
                if msg.starts_with("BEACON|") {
                    let parts: Vec<&str> = msg.trim().split('|').collect();
                    if parts.len() == 3 {
                        let name = parts[1].to_string();
                        let ip = parts[2].to_string();
                        let now = now_secs();
                        let mut devices = state_receiver.devices.blocking_write();
                        if let Some(pos) = devices.iter().position(|d| d.ip == ip) {
                            devices[pos].last_seen = now;
                        } else {
                            devices.push(DeviceInfo { name, ip, last_seen: now });
                        }
                        devices.retain(|d| now.saturating_sub(d.last_seen) < 10);
                    }
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Err(e) = tauri::Builder::default()
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
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        eprintln!("[Server] Не удалось создать tokio runtime: {}", e);
                        return;
                    }
                };
                runtime.block_on(async {
                    let ip = state_clone.get_active_ip().await;
                    let router = server::create_router(state_clone.clone());
                    let listener = match tokio::net::TcpListener::bind("0.0.0.0:7431").await {
                        Ok(listener) => listener,
                        Err(e) => {
                            eprintln!("[Server] Не удалось слушать порт 7431: {}", e);
                            return;
                        }
                    };
                    println!("[Server] FileHop HTTP running on http://{}:7431", ip);

                    let cleanup_state = state_clone.clone();
                    tokio::spawn(async move {
                        let mut interval = tokio::time::interval(Duration::from_secs(60));
                        loop {
                            interval.tick().await;
                            cleanup_state.cleanup_expired_downloads().await;
                        }
                    });

                    if let Err(e) = axum::serve(
                        listener,
                        router.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .await
                    {
                        eprintln!("[Server] Ошибка HTTP-сервера: {}", e);
                    }
                });
            });
            Ok(())
        })
        .run(tauri::generate_context!())
    {
        eprintln!("[FileHop] Ошибка запуска приложения: {}", e);
    }
}
