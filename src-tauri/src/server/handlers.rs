use axum::{
    extract::{multipart::Field, multipart::MultipartError, ConnectInfo, Query, State, Multipart},
    http::{header, StatusCode},
    response::{Html, Response},
    body::Body,
    Json,
};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use crate::server::{models::*, state::AppState};
use tauri::Emitter;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

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
        err.body_text()
    };
    eprintln!("[Upload] Ошибка multipart ({}): {} | {}", status, err, message);
    (status, message)
}

fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ' | '(' | ')' | '[' | ']') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '.' || c == ' ').to_string();
    if cleaned.is_empty() {
        "unknown_file".to_string()
    } else {
        cleaned
    }
}

/// Читает поле multipart кусками и сразу пишет их на диск.
/// Возвращает (безопасное имя, путь к временному файлу, размер).
async fn save_file_field(
    state: &AppState,
    mut field: Field<'_>,
    prefix: &str,
) -> Result<(String, PathBuf, usize), (StatusCode, String)> {
    let original_name = field.file_name().unwrap_or("unknown_file").to_string();
    let filename = sanitize_filename(&original_name);
    let token = format!("{:016x}", rand::random::<u64>());
    let mut temp_path = state.temp_dir.clone();
    temp_path.push(format!("{}_{}_{}", prefix, token, filename));

    let mut file = File::create(&temp_path).await.map_err(|e| {
        eprintln!("[Upload] Не удалось создать {}: {}", temp_path.display(), e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Ошибка создания файла: {}", e),
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
        if let Err(e) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            eprintln!("[Upload] Ошибка записи {}: {}", temp_path.display(), e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Ошибка записи: {}", e),
            ));
        }
        written += chunk.len();
    }

    if let Err(e) = file.flush().await {
        drop(file);
        let _ = tokio::fs::remove_file(&temp_path).await;
        eprintln!("[Upload] Ошибка записи {}: {}", temp_path.display(), e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Ошибка записи: {}", e),
        ));
    }

    Ok((filename, temp_path, written))
}

pub async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let code = state.access_code.read().await.clone();
    let local_ip = state.get_active_ip().await;

    Json(StatusResponse {
        status: "online".to_string(),
        code,
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

    let file = File::open(&download.temp_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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

/// Модель доверия: /upload_pc доступен всем устройствам локальной сети без кода.
/// Это осознанный выбор для рабочего сценария ПК-ПК; код требуется только на /upload
/// для загрузки с телефона. Не добавлять проверку кода без изменения протокола ПК-ПК.
pub async fn handle_pc_upload(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    mut multipart: Multipart,
) -> UploadResult {
    println!("[PC-to-PC] Получен файл из локальной сети от {}", addr);
    let mut saved: Option<(String, PathBuf, usize)> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => return Err(multipart_error(err)),
        };
        if field.name().unwrap_or("") == "file" {
            let result = save_file_field(&state, field, "pc").await?;
            println!("[PC-to-PC] Файл: {} ({} байт)", result.0, result.2);
            saved = Some(result);
        }
    }

    if let Some((filename, temp_path, size)) = saved {
        let payload = FileReceivedPayload {
            filename: filename.clone(),
            temp_path: temp_path.to_string_lossy().to_string(),
            size,
        };

        if state.app_handle.emit("file_received", payload).is_err() {
            eprintln!("[PC-to-PC] Не удалось отправить событие file_received");
        }
        println!("[PC-to-PC] Файл получен и ждет сохранения: {}", filename);
        return Ok((StatusCode::OK, "Файл успешно доставлен".to_string()));
    }

    Err((
        StatusCode::BAD_REQUEST,
        "Файл не был прикреплен".to_string(),
    ))
}

pub async fn serve_mobile_page() -> Html<String> {
    Html(
        r#"
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
            Внимание: передача идет по локальной сети (HTTP). Если браузер блокирует скачивание, нажмите "Сохранить" или "Все равно скачать".
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
                const statusDiv = document.getElementById('status');
                const btn = document.getElementById('sendBtn');
                const track = document.getElementById('progressTrack');
                const fill = document.getElementById('progressFill');
                if (!code || code.length !== 6) { statusDiv.innerHTML = '<span class="error">Введите 6-значный код!</span>'; return; }
                if (fileInput.files.length === 0) { statusDiv.innerHTML = '<span class="error">Выберите файл!</span>'; return; }
                btn.disabled = true;
                statusDiv.textContent = 'Отправка... 0%';
                fill.style.width = '0%';
                track.classList.remove('indeterminate');
                track.style.display = 'block';

                const formData = new FormData();
                formData.append('code', code);
                formData.append('file', fileInput.files[0]);
                const xhr = new XMLHttpRequest();
                xhr.open('POST', '/upload');
                xhr.upload.onprogress = function (event) {
                    if (event.lengthComputable && event.total > 0) {
                        const percent = Math.min(100, Math.floor(event.loaded * 100 / event.total));
                        fill.style.width = percent + '%';
                        statusDiv.textContent = 'Отправка... ' + percent + '%';
                    } else {
                        track.classList.add('indeterminate');
                        statusDiv.textContent = 'Отправка...';
                    }
                };
                xhr.onload = function () {
                    track.style.display = 'none';
                    btn.disabled = false;
                    const text = xhr.responseText || (xhr.status >= 400 ? 'Ошибка сервера' : 'Файл получен');
                    statusDiv.innerHTML = xhr.status >= 200 && xhr.status < 300
                        ? '<span class="success">' + text + '</span>'
                        : '<span class="error">Ошибка: ' + text + '</span>';
                    if (xhr.status >= 200 && xhr.status < 300) fileInput.value = '';
                };
                xhr.onerror = function () {
                    track.style.display = 'none';
                    btn.disabled = false;
                    statusDiv.innerHTML = '<span class="error">Ошибка сети при отправке файла</span>';
                };
                xhr.onabort = function () {
                    track.style.display = 'none';
                    btn.disabled = false;
                    statusDiv.innerHTML = '<span class="error">Отправка отменена</span>';
                };
                xhr.send(formData);
            }
        </script>
    </body>
    </html>
    "#
        .replace("__MAX_UPLOAD_BYTES__", &(MAX_UPLOAD_SIZE as u64).to_string()),
    )
}

pub async fn handle_upload(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    mut multipart: Multipart,
) -> UploadResult {
    let mut received_code = String::new();
    let mut saved: Option<(String, PathBuf, usize)> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => return Err(multipart_error(err)),
        };
        let name = field.name().unwrap_or("").to_string();
        if name == "code" {
            received_code = field.text().await.map_err(multipart_error)?;
        } else if name == "file" {
            let result = save_file_field(&state, field, "up").await?;
            println!("[Upload] Файл: {} ({} байт)", result.0, result.2);
            saved = Some(result);
        }
    }

    let current_code = state.access_code.read().await.clone();
    if received_code != current_code {
        let ip = addr.ip().to_string();
        eprintln!("[Upload] Неверный код с {}", ip);
        if !state.check_rate_limit(&ip).await {
            eprintln!("[Upload] Превышен лимит попыток с {}", ip);
            if let Some((_, path, _)) = saved {
                let _ = tokio::fs::remove_file(path).await;
            }
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "Слишком много неверных попыток. Попробуйте позже.".to_string(),
            ));
        }
        if let Some((_, path, _)) = saved {
            let _ = tokio::fs::remove_file(path).await;
        }
        return Err((StatusCode::UNAUTHORIZED, "Неверный код!".to_string()));
    }

    if let Some((filename, temp_path, size)) = saved {
        let payload = FileReceivedPayload {
            filename: filename.clone(),
            temp_path: temp_path.to_string_lossy().to_string(),
            size,
        };
        if state.app_handle.emit("file_received", payload).is_err() {
            eprintln!("[Upload] Не удалось отправить событие file_received");
        }
        println!("[Upload] Файл получен и ждет сохранения: {}", filename);
        return Ok((StatusCode::OK, "Файл получен".to_string()));
    }

    Err((StatusCode::BAD_REQUEST, "Файл не прикреплен".to_string()))
}
