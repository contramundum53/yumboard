use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;

mod handlers;
mod logic;
mod sessions;
mod state;

use crate::handlers::{root_handler, session_handler, ws_handler};
use crate::state::AppState;

#[tokio::main]
async fn main() {
    let session_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sessions");
    if let Err(error) = tokio::fs::create_dir_all(&session_dir).await {
        eprintln!("Failed to create session dir: {error}");
    }
    let state = AppState {
        sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        session_dir,
    };

    let public_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../public");
    let index_file = public_dir.join("index.html");

    let app = Router::new()
        .route("/", get(root_handler))
        .route("/s/:session_id", get(session_handler))
        .route("/ws/:session_id", get(ws_handler))
        .fallback_service(ServeDir::new(public_dir).append_index_html_on_directories(true))
        .layer(axum::Extension(index_file))
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("Whiteboard running at http://localhost:{port}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind server");
    axum::serve(listener, app)
        .await
        .expect("Server crashed");
}
