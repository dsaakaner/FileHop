use axum::{
    extract::{Query, State, Multipart},
    http::{header, StatusCode},
    response::{Html, Response},
    body::Body,
    Json,
};
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use crate::server::{models::*, state::AppState};
use tauri::Emitter;
use std::collections::HashMap;

pub async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let code = state.access_code.read().await.clone();
    let clients = state.connected_clients.read().await;
    let local_ip = state.get_active_ip().await;
    
    Json(StatusResponse {
        status: "online".to_string(),
        code,
        local_ip,
        clients_count: *clients,
    })
}

pub async fn handle_download(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response<Body>, StatusCode> {
    let token = params.get("token").ok_or(StatusCode::BAD_REQUEST)?;
    let download = state.get_pending_download(token).await.ok_or(StatusCode::NOT_FOUND)?;
    
    let file = File::open(&download.temp_path).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);
    
    let filename_header = format!("attachment; filename=\"{}\"", download.filename);
    
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_DISPOSITION, filename_header)
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
}

pub async fn handle_pc_upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> (StatusCode, String) {
    println!("[PC-to-PC] Получен файл из локальной сети");
    let mut file_data: Option<(String, Vec<u8>)> = None;

    while let Some(field) = multipart.next_field().await.unwrap() {
        if field.name().unwrap_or("") == "file" {
            let filename = field.file_name().unwrap_or("unknown_file").to_string();
            let data = field.bytes().await.unwrap_or_default().to_vec();
            println!("  Файл: {} ({} байт)", filename, data.len());
            file_data = Some((filename, data));
        }
    }

    if let Some((filename, data)) = file_data {
        let temp_path = state.temp_dir.join(&filename);
        if let Err(e) = std::fs::write(&temp_path, &data) {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Ошибка записи: {}", e));
        }

        let payload = FileReceivedPayload {
            filename: filename.clone(),
            temp_path: temp_path.to_string_lossy().to_string(),
            size: data.len(),
        };
        
        let _ = state.app_handle.emit("file_received", payload);
        println!("[PC-to-PC] Файл получен и ждет сохранения: {}", filename);
        return (StatusCode::OK, "Файл успешно доставлен".to_string());
    }

    (StatusCode::BAD_REQUEST, "Файл не был прикреплен".to_string())
}

pub async fn serve_mobile_page(State(state): State<AppState>) -> Html<&'static str> {
    {
        let mut clients = state.connected_clients.write().await;
        if *clients < 100 { *clients += 1; }
    }
    Html(r#"
    <!DOCTYPE html>
    <html>
    <head>
        <meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no">
        <title>FileHop Web</title>
        <style>
            body { font-family: system-ui, sans-serif; text-align: center; padding: 20px; background: #f4f4f5; color: #333; }
            h1 { color: #2563eb; }
            input[type="text"] { padding: 12px; font-size: 1.5rem; width: 80%; max-width: 250px; margin: 15px 0; text-align: center; letter-spacing: 0.3rem; border: 2px solid #ddd; border-radius: 8px; }
            input[type="file"] { margin: 15px 0; padding: 10px; background: white; border-radius: 8px; width: 90%; }
            button { padding: 14px 28px; font-size: 1.1rem; background: #2563eb; color: white; border: none; border-radius: 8px; cursor: pointer; font-weight: bold; width: 80%; max-width: 250px; }
            button:disabled { background: #9ca3af; cursor: not-allowed; }
            #status { margin-top: 20px; font-weight: bold; min-height: 24px; }
            .success { color: #16a34a; } .error { color: #dc2626; }
            .warning { background: #fff3cd; color: #856404; padding: 10px; border-radius: 8px; font-size: 0.85rem; margin-bottom: 15px; border: 1px solid #ffeeba; }
        </style>
    </head>
    <body>
        <h1>FileHop Web</h1>
        <div class="warning">
            Внимание: передача идет по локальной сети (HTTP). Если браузер блокирует скачивание, нажмите "Сохранить" или "Все равно скачать".
        </div>
        <p>Код с компьютера:</p>
        <input type="text" maxlength="6" placeholder="000000" inputmode="numeric" id="code" />
        <br><p>Выберите файл:</p>
        <input type="file" id="file" /><br>
        <button id="sendBtn" onclick="sendFile()">Отправить на ПК</button>
        <div id="status"></div>
        <script>
            const urlParams = new URLSearchParams(window.location.search);
            if (urlParams.get('code')) document.getElementById('code').value = urlParams.get('code');
            async function sendFile() {
                const code = document.getElementById('code').value;
                const fileInput = document.getElementById('file');
                const statusDiv = document.getElementById('status');
                const btn = document.getElementById('sendBtn');
                if (!code || code.length !== 6) { statusDiv.innerHTML = '<span class="error">Введите 6-значный код!</span>'; return; }
                if (fileInput.files.length === 0) { statusDiv.innerHTML = '<span class="error">Выберите файл!</span>'; return; }
                btn.disabled = true; statusDiv.innerHTML = 'Отправка...';
                const formData = new FormData();
                formData.append('code', code); formData.append('file', fileInput.files[0]);
                try {
                    const response = await fetch('/upload', { method: 'POST', body: formData });
                    const text = await response.text();
                    statusDiv.innerHTML = response.ok ? '<span class="success">✅ ' + text + '</span>' : '<span class="error">Ошибка: ' + text + '</span>';
                    if (response.ok) fileInput.value = '';
                } catch (err) { statusDiv.innerHTML = '<span class="error">Ошибка сети</span>'; }
                finally { btn.disabled = false; }
            }
        </script>
    </body>
    </html>
    "#)
}

pub async fn handle_upload(State(state): State<AppState>, mut multipart: Multipart) -> (StatusCode, String) {
    // Здесь можно было бы получить IP из запроса, но для простоты пока оставим базовую проверку
    let mut received_code = String::new();
    let mut file_data: Option<(String, Vec<u8>)> = None;
    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap_or("").to_string();
        if name == "code" { received_code = field.text().await.unwrap_or_default(); }
        else if name == "file" {
            let filename = field.file_name().unwrap_or("unknown_file").to_string();
            let data = field.bytes().await.unwrap_or_default().to_vec();
            file_data = Some((filename, data));
        }
    }
    let current_code = state.access_code.read().await.clone();
    if received_code != current_code { 
        // Здесь можно добавить rate limiting по IP, если передать IP в State
        return (StatusCode::UNAUTHORIZED, "Неверный код!".to_string()); 
    }
    if let Some((filename, data)) = file_data {
        let temp_path = state.temp_dir.join(&filename);
        if let Err(e) = std::fs::write(&temp_path, &data) { return (StatusCode::INTERNAL_SERVER_ERROR, format!("Ошибка: {}", e)); }
        let payload = FileReceivedPayload { filename: filename.clone(), temp_path: temp_path.to_string_lossy().to_string(), size: data.len() };
        let _ = state.app_handle.emit("file_received", payload);
        return (StatusCode::OK, "Файл получен".to_string());
    }
    (StatusCode::BAD_REQUEST, "Файл не прикреплен".to_string())
}
