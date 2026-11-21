use crate::server::{
    filename::{content_disposition_attachment, sanitize_filename_component},
    models::*,
    state::{AppState, MAX_PC_UPLOAD_SIZE, PC_UPLOAD_CONFIRM_TIMEOUT_SECS},
};
use axum::{
    Json,
    body::Body,
    extract::{ConnectInfo, Multipart, Query, State, multipart::Field, multipart::MultipartError},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use tauri::Emitter;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

/// Лимит тела запроса. Потоковая запись позволяет держать это ограничение
/// на уровне 4 ГиБ без загрузки всего файла в память.
pub const MAX_UPLOAD_SIZE: usize = 4 * 1024 * 1024 * 1024;

type UploadResult = Result<(StatusCode, String), (StatusCode, String)>;

fn multipart_error(err: MultipartError) -> (StatusCode, String) {
    let status = err.status();
    let message = if status == StatusCode::PAYLOAD_TOO_LARGE {
        format!(
            "Файл слишком большой. Максимальный размер: {} МБ.",
            MAX_UPLOAD_SIZE / 1024 / 1024
        )
    } else {
        "Ошибка при передаче файла".to_string()
    };
    eprintln!("[Upload] Ошибка multipart ({}): {}", status, err);
    (status, message)
}

/// Читает поле multipart кусками и сразу пишет их на диск.
/// Возвращает (безопасное имя, путь к временному файлу, размер).
async fn save_file_field(
    state: &AppState,
    mut field: Field<'_>,
    prefix: &str,
    max_size: usize,
) -> Result<(String, PathBuf, usize), (StatusCode, String)> {
    let original_name = field.file_name().unwrap_or("unknown_file").to_string();
    let filename = sanitize_filename_component(&original_name);
    let token = format!("{:016x}", rand::random::<u64>());
    let mut temp_path = state.temp_dir.clone();
    temp_path.push(format!("{}_{}_{}", prefix, token, filename));

    let mut file = File::create(&temp_path).await.map_err(|e| {
        eprintln!("[Upload] Не удалось создать файл: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Ошибка создания файла".to_string(),
        )
    })?;

    let mut written = 0usize;
    loop {
        let chunk = match field.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(err) => {
                drop(file);
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(multipart_error(err));
            }
        };
        written += chunk.len();
        if written > max_size {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Файл слишком большой. Максимальный размер: {} МБ.",
                    max_size / 1024 / 1024
                ),
            ));
        }
        if let Err(e) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            eprintln!("[Upload] Ошибка записи: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Ошибка записи файла".to_string(),
            ));
        }
    }

    if let Err(e) = file.flush().await {
        drop(file);
        let _ = tokio::fs::remove_file(&temp_path).await;
        eprintln!("[Upload] Ошибка записи: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Ошибка записи файла".to_string(),
        ));
    }

    Ok((filename, temp_path, written))
}

pub async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let local_ip = state.get_active_ip().await;

    Json(StatusResponse {
        status: "online".to_string(),
        local_ip,
    })
}

pub async fn handle_download(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response<Body>, StatusCode> {
    let token = params.get("token").ok_or(StatusCode::BAD_REQUEST)?;
    let download = state
        .get_pending_download(token)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let path = std::path::PathBuf::from(&download.source_path);
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let file = File::open(&path).await.map_err(|_| StatusCode::NOT_FOUND)?;
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);
    let size = meta.len();
    let filename_header = content_disposition_attachment(&download.filename);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_DISPOSITION, filename_header)
        .header(header::CONTENT_LENGTH, size.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn drain_multipart(mut multipart: Multipart) {
    loop {
        match multipart.next_field().await {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }
}

/// Загрузка файла с другого ПК. Требуется заголовок протокола новой версии;
/// старые клиенты (v0.1.1 и ниже) получают явный отказ.
pub async fn handle_pc_upload(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> UploadResult {
    let version = headers
        .get("x-filehop-pc-upload")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if version != "2" {
        println!(
            "[PC-to-PC] Отклонен устаревший или несовместимый клиент от {}",
            addr.ip()
        );
        drain_multipart(multipart).await;
        return Err((
            StatusCode::UPGRADE_REQUIRED,
            "Несовместимая версия FileHop. Обновите приложение на обеих компьютерах до актуальной версии и повторите отправку."
                .to_string(),
        ));
    }

    let sender_name = headers
        .get("x-filehop-sender-name")
        .and_then(|v| v.to_str().ok())
        .map(|s| sanitize_filename_component(s.trim()))
        .filter(|s| !s.is_empty() && s != "unknown_file");
    let sender_ip = addr.ip().to_string();

    let mut saved: Option<(String, PathBuf, usize)> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => {
                if let Some((_, path, _)) = saved {
                    let _ = tokio::fs::remove_file(path).await;
                }
                return Err(multipart_error(err));
            }
        };
        if field.name().unwrap_or("") == "file" {
            let result = save_file_field(&state, field, "pcstage", MAX_PC_UPLOAD_SIZE).await;
            match result {
                Ok(r) => {
                    if let Some((_, old_path, _)) = saved.take() {
                        let _ = tokio::fs::remove_file(old_path).await;
                    }
                    saved = Some(r);
                }
                Err(e) => {
                    if let Some((_, path, _)) = saved {
                        let _ = tokio::fs::remove_file(path).await;
                    }
                    drain_multipart(multipart).await;
                    return Err(e);
                }
            }
        }
    }

    let Some((filename, temp_path, size)) = saved else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Файл не был прикреплен".to_string(),
        ));
    };

    let id = format!("{:016x}", rand::random::<u64>());
    let request = IncomingTransferRequest {
        id: id.clone(),
        filename: filename.clone(),
        size,
        sender_ip: sender_ip.clone(),
        sender_name,
    };

    let rx = state.register_pc_decision(id.clone()).await;
    let emitted = if let Some(h) = &state.app_handle {
        h.emit("pc-incoming-request", request).is_ok()
    } else {
        false
    };
    if !emitted {
        let _ = state.resolve_pc_decision(&id, false).await;
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Не удалось показать запрос на подтверждение получателю".to_string(),
        ));
    }

    let accepted = match tokio::time::timeout(
        std::time::Duration::from_secs(PC_UPLOAD_CONFIRM_TIMEOUT_SECS),
        rx,
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = state.resolve_pc_decision(&id, false).await;
            false
        }
    };

    if !accepted {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err((
            StatusCode::FORBIDDEN,
            "Получатель отклонил передачу или время ожидания истекло".to_string(),
        ));
    }

    let payload = FileReceivedPayload {
        id: format!("{:016x}", rand::random::<u64>()),
        filename: filename.clone(),
        temp_path: temp_path.to_string_lossy().to_string(),
        size,
        source: "pc".to_string(),
        sender_ip: Some(sender_ip),
    };
    state.enqueue_incoming(payload).await;
    if let Some(h) = &state.app_handle {
        let _ = h.emit("incoming-queue-updated", ());
    }

    println!("[PC-to-PC] Файл принят получателем: {}", filename);
    Ok((StatusCode::OK, "Файл принят получателем".to_string()))
}

pub async fn serve_mobile_page() -> Response {
    let html = r##"
    <!DOCTYPE html>
    <html>
    <head>
        <meta name="viewport" content="width=device-width, initial-scale=1.0">
        <meta http-equiv="Cache-Control" content="no-store, no-cache, must-revalidate, max-age=0">
        <meta http-equiv="Pragma" content="no-cache">
        <meta http-equiv="Expires" content="0">
        <title>FileHop Web</title>
        <style>
            body { font-family: system-ui, sans-serif; text-align: center; padding: 20px; background: #f4f4f5; color: #333; }
            h1 { color: #2563eb; }
            input[type="text"] { padding: 12px; font-size: 1.5rem; width: 80%; max-width: 250px; margin: 15px 0; text-align: center; letter-spacing: 0.3rem; border: 2px solid #ddd; border-radius: 8px; }
            input[type="file"] { margin: 15px 0; padding: 10px; background: white; border-radius: 8px; width: 90%; }
            button { padding: 14px 28px; font-size: 1.1rem; background: #2563eb; color: white; border: none; border-radius: 8px; cursor: pointer; font-weight: bold; width: 80%; max-width: 250px; }
            button:disabled { background: #9ca3af; cursor: not-allowed; }
            #status { margin-top: 20px; font-weight: bold; min-height: 24px; }
            .progress-track { display: none; width: 80%; max-width: 250px; height: 8px; margin: 15px auto 0; background: #e4e4e7; border-radius: 4px; overflow: hidden; }
            .progress-fill { width: 0; height: 100%; background: #2563eb; border-radius: 4px; transition: width 0.2s ease; }
            .progress-track.indeterminate .progress-fill { width: 40% !important; animation: progress-slide 1s ease-in-out infinite; }
            @keyframes progress-slide { 0% { margin-left: -40%; } 100% { margin-left: 100%; } }
            .success { color: #16a34a; } .error { color: #dc2626; }
            .warning { background: #fff3cd; color: #856404; padding: 10px; border-radius: 8px; font-size: 0.85rem; margin-bottom: 15px; border: 1px solid #ffeeba; }
        </style>
    </head>
    <body>
        <h1>FileHop Web</h1>
        <div class="warning">
            Внимание: передача идет по локальной сети (HTTP). Данные и код доступа могут быть видны в сети.
        </div>
        <p>Код с компьютера:</p>
        <input type="text" maxlength="6" placeholder="000000" inputmode="numeric" id="code" />
        <br><p>Выберите файл:</p>
        <input type="file" id="file" onchange="checkFileSize()" /><br>
        <div id="sizeWarning" class="warning" style="display:none"></div>
        <button id="sendBtn" onclick="sendFile()">Отправить на ПК</button>
        <div id="progressTrack" class="progress-track"><div id="progressFill" class="progress-fill"></div></div>
        <div id="status"></div>
        <script>
            const MAX_UPLOAD_BYTES = __MAX_UPLOAD_BYTES__;
            const urlParams = new URLSearchParams(window.location.search);
            if (urlParams.get('code')) document.getElementById('code').value = urlParams.get('code');

            function setStatus(text, kind) {
                const statusDiv = document.getElementById('status');
                statusDiv.textContent = text || '';
                statusDiv.className = kind || '';
            }

            function checkFileSize() {
                const file = document.getElementById('file').files[0];
                const warn = document.getElementById('sizeWarning');
                if (file && file.size > MAX_UPLOAD_BYTES) {
                    warn.textContent = 'Файл больше ' + Math.round(MAX_UPLOAD_BYTES / 1048576) + ' МБ. Передача может завершиться ошибкой.';
                    warn.style.display = 'block';
                } else {
                    warn.style.display = 'none';
                }
            }

            function sendFile() {
                const code = document.getElementById('code').value;
                const fileInput = document.getElementById('file');
                const btn = document.getElementById('sendBtn');
                const track = document.getElementById('progressTrack');
                const fill = document.getElementById('progressFill');
                if (!code || code.length !== 6) { setStatus('Введите 6-значный код!', 'error'); return; }
                if (fileInput.files.length === 0) { setStatus('Выберите файл!', 'error'); return; }
                btn.disabled = true;
                setStatus('Отправка... 0%', '');
                fill.style.width = '0%';
                track.classList.remove('indeterminate');
                track.style.display = 'block';

                const formData = new FormData();
                formData.append('code', code);
                formData.append('file', fileInput.files[0]);
                const xhr = new XMLHttpRequest();
                xhr.open('POST', '/upload');
                xhr.setRequestHeader('X-Access-Code', code);
                xhr.upload.onprogress = function (event) {
                    if (event.lengthComputable && event.total > 0) {
                        const percent = Math.min(100, Math.floor(event.loaded * 100 / event.total));
                        fill.style.width = percent + '%';
                        setStatus('Отправка... ' + percent + '%', '');
                    } else {
                        track.classList.add('indeterminate');
                        setStatus('Отправка...', '');
                    }
                };
                xhr.onload = function () {
                    track.style.display = 'none';
                    btn.disabled = false;
                    const ok = xhr.status >= 200 && xhr.status < 300;
                    const text = xhr.responseText || (ok ? 'Файл получен' : 'Ошибка сервера');
                    if (ok) {
                        setStatus(text, 'success');
                        fileInput.value = '';
                        document.getElementById('sizeWarning').style.display = 'none';
                    } else {
                        setStatus('Ошибка: ' + text, 'error');
                    }
                };
                xhr.onerror = function () {
                    track.style.display = 'none';
                    btn.disabled = false;
                    setStatus('Ошибка сети при отправке файла', 'error');
                };
                xhr.onabort = function () {
                    track.style.display = 'none';
                    btn.disabled = false;
                    setStatus('Отправка отменена', 'error');
                };
                xhr.send(formData);
            }
        </script>
    </body>
    </html>
    "##.replace("__MAX_UPLOAD_BYTES__", &(MAX_UPLOAD_SIZE as u64).to_string());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate, max-age=0")
        .header(header::PRAGMA, "no-cache")
        .header(header::EXPIRES, "0")
        .header(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'self'",
        )
        .body(Body::from(html))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "error").into_response())
}

pub async fn handle_upload(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> UploadResult {
    let header_code = headers
        .get("x-access-code")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let current_code = state.access_code.read().await.clone();
    if let Some(ref provided) = header_code
        && provided != &current_code
    {
        let ip = addr.ip().to_string();
        if !state.check_rate_limit(&ip).await {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "Слишком много неверных попыток. Попробуйте позже.".to_string(),
            ));
        }
        return Err((StatusCode::UNAUTHORIZED, "Неверный код!".to_string()));
    }

    let mut received_code = header_code.unwrap_or_default();
    let mut saved: Option<(String, PathBuf, usize)> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => return Err(multipart_error(err)),
        };
        let name = field.name().unwrap_or("").to_string();
        if name == "code" {
            let code = field
                .text()
                .await
                .map_err(multipart_error)?
                .trim()
                .to_string();
            if !received_code.is_empty() && received_code != code {
                if let Some((_, path, _)) = saved {
                    let _ = tokio::fs::remove_file(path).await;
                }
                let ip = addr.ip().to_string();
                if !state.check_rate_limit(&ip).await {
                    return Err((
                        StatusCode::TOO_MANY_REQUESTS,
                        "Слишком много неверных попыток. Попробуйте позже.".to_string(),
                    ));
                }
                return Err((StatusCode::UNAUTHORIZED, "Неверный код!".to_string()));
            }
            if received_code.is_empty() {
                if code != current_code {
                    if let Some((_, path, _)) = saved {
                        let _ = tokio::fs::remove_file(path).await;
                    }
                    let ip = addr.ip().to_string();
                    if !state.check_rate_limit(&ip).await {
                        return Err((
                            StatusCode::TOO_MANY_REQUESTS,
                            "Слишком много неверных попыток. Попробуйте позже.".to_string(),
                        ));
                    }
                    return Err((StatusCode::UNAUTHORIZED, "Неверный код!".to_string()));
                }
                received_code = code;
            }
        } else if name == "file" {
            if received_code != current_code {
                // Код не передан в заголовке и поле code ещё не встретилось — не пишем файл на диск.
                let ip = addr.ip().to_string();
                if !state.check_rate_limit(&ip).await {
                    return Err((
                        StatusCode::TOO_MANY_REQUESTS,
                        "Слишком много неверных попыток. Попробуйте позже.".to_string(),
                    ));
                }
                return Err((StatusCode::UNAUTHORIZED, "Неверный код!".to_string()));
            }
            let result = save_file_field(&state, field, "up", MAX_UPLOAD_SIZE).await;
            match result {
                Ok(r) => {
                    if let Some((_, old_path, _)) = saved.take() {
                        let _ = tokio::fs::remove_file(old_path).await;
                    }
                    println!("[Upload] Получен файл ({} байт)", r.2);
                    saved = Some(r);
                }
                Err(e) => {
                    if let Some((_, path, _)) = saved {
                        let _ = tokio::fs::remove_file(path).await;
                    }
                    return Err(e);
                }
            }
        }
    }

    if received_code != current_code {
        if let Some((_, path, _)) = saved {
            let _ = tokio::fs::remove_file(path).await;
        }
        let ip = addr.ip().to_string();
        if !state.check_rate_limit(&ip).await {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "Слишком много неверных попыток. Попробуйте позже.".to_string(),
            ));
        }
        return Err((StatusCode::UNAUTHORIZED, "Неверный код!".to_string()));
    }

    if let Some((filename, temp_path, size)) = saved {
        let payload = FileReceivedPayload {
            id: format!("{:016x}", rand::random::<u64>()),
            filename: filename.clone(),
            temp_path: temp_path.to_string_lossy().to_string(),
            size,
            source: "phone".to_string(),
            sender_ip: Some(addr.ip().to_string()),
        };
        state.enqueue_incoming(payload).await;
        if let Some(h) = &state.app_handle {
            let _ = h.emit("incoming-queue-updated", ());
        }
        println!("[Upload] Файл получен и ждет сохранения: {}", filename);
        return Ok((StatusCode::OK, "Файл получен".to_string()));
    }

    Err((StatusCode::BAD_REQUEST, "Файл не прикреплен".to_string()))
}
