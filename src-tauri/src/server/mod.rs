pub mod handlers;
pub mod models;
pub mod state;

use axum::{Router, routing::{get, post}};
use tower_http::cors::{CorsLayer, Any};
use crate::server::handlers::*;
use crate::server::state::AppState;

pub fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    Router::new()
        .route("/", get(serve_mobile_page))
        .route("/upload", post(handle_upload))
        .route("/upload_pc", post(handle_pc_upload))
        .route("/download", get(handle_download))
        .route("/api/status", get(get_status))
        .layer(cors)
        .with_state(state)
}
