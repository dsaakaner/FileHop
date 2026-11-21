use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn make_state() -> (
    filehop_temp_lib::server::state::AppState,
    std::path::PathBuf,
) {
    let dir = std::env::temp_dir().join(format!("filehop_router_test_{}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = filehop_temp_lib::server::state::AppState::new_headless(dir.clone());
    (state, dir)
}

async fn body_text(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

#[tokio::test]
async fn api_status_does_not_expose_code() {
    let (state, dir) = make_state();
    let app = filehop_temp_lib::server::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let text = body_text(resp.into_body()).await;
    assert!(!text.contains("code"));
    assert!(text.contains("status"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn upload_without_code_returns_401_and_writes_no_file() {
    let (state, dir) = make_state();
    let temp_dir = state.get_temp_dir();
    let app = filehop_temp_lib::server::create_router(state);
    let boundary = "BOUNDARY123";
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.txt\"\r\nContent-Type: text/plain\r\n\r\nsecret\r\n--{b}--\r\n",
        b = boundary
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/upload")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let entries: Vec<_> = std::fs::read_dir(&temp_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        entries.is_empty(),
        "temp dir should be empty, got {:?}",
        entries
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn upload_with_wrong_code_returns_401_and_writes_no_file() {
    let (state, dir) = make_state();
    let temp_dir = state.get_temp_dir();
    let app = filehop_temp_lib::server::create_router(state.clone());
    let boundary = "BOUNDARY123";
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.txt\"\r\nContent-Type: text/plain\r\n\r\nsecret\r\n--{b}--\r\n",
        b = boundary
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/upload")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .header("x-access-code", "000000")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status() == StatusCode::UNAUTHORIZED || resp.status() == StatusCode::TOO_MANY_REQUESTS
    );
    let entries: Vec<_> = std::fs::read_dir(&temp_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(entries.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn upload_with_correct_code_succeeds_and_enqueues() {
    let (state, dir) = make_state();
    let code = state.access_code.read().await.clone();
    let app = filehop_temp_lib::server::create_router(state.clone());
    let boundary = "BOUNDARY123";
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"code\"\r\n\r\n{code}\r\n--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"report.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--{b}--\r\n",
        b = boundary,
        code = code
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/upload")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let q = state.incoming_queue.lock().await;
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].payload.filename, "report.txt");
    let temp_path = std::path::PathBuf::from(&q[0].payload.temp_path);
    drop(q);
    assert!(temp_path.exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn upload_pc_without_version_returns_426() {
    let (state, dir) = make_state();
    let app = filehop_temp_lib::server::create_router(state);
    let boundary = "BOUNDARY123";
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.txt\"\r\nContent-Type: application/octet-stream\r\n\r\ndata\r\n--{b}--\r\n",
        b = boundary
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/upload_pc")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UPGRADE_REQUIRED);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn download_with_valid_token_streams_file() {
    let (state, dir) = make_state();
    let file_path = dir.join("source.txt");
    std::fs::write(&file_path, b"hello world").unwrap();
    let token = "tok123".to_string();
    state
        .add_pending_download(
            token.clone(),
            "source.txt".to_string(),
            file_path.to_string_lossy().to_string(),
            11,
            false,
        )
        .await;
    let app = filehop_temp_lib::server::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/download?token={}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let headers = resp.headers().clone();
    assert!(headers.get("content-length").is_some());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"hello world");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn download_with_invalid_token_returns_404() {
    let (state, dir) = make_state();
    let app = filehop_temp_lib::server::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/download?token=missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(dir);
}
