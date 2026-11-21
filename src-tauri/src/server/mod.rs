pub mod filename;
pub mod handlers;
pub mod models;
pub mod state;

use crate::server::handlers::*;
use crate::server::state::AppState;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(serve_mobile_page))
        .route("/upload", post(handle_upload))
        .route("/upload_pc", post(handle_pc_upload))
        .route("/download", get(handle_download))
        .route("/api/status", get(get_status))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_SIZE))
        .with_state(state)
}
