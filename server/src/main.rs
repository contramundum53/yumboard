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
use crate::sessions::save_session;
use crate::state::AppState;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let session_dir = parse_path_arg(&args, "--session-dir")
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sessions"));
    if let Err(error) = tokio::fs::create_dir_all(&session_dir).await {
        eprintln!("Failed to create session dir: {error}");
    }
    let state = AppState {
        sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        session_dir,
    };
    let backup_state = state.clone();

    let public_dir = parse_path_arg(&args, "--public-dir")
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../public"));
    let index_file = public_dir.join("index.html");

    let app = Router::new()
        .route("/", get(root_handler))
        .route("/s/:session_id", get(session_handler))
        .route("/ws/:session_id", get(ws_handler))
        .fallback_service(ServeDir::new(public_dir).append_index_html_on_directories(true))
        .layer(axum::Extension(index_file))
        .with_state(state);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let sessions = {
                let sessions = backup_state.sessions.read().await;
                sessions
                    .iter()
                    .map(|(session_id, session)| (session_id.clone(), session.clone()))
                    .collect::<Vec<_>>()
            };
            for (session_id, session) in sessions {
                let maybe_strokes = {
                    let mut session = session.write().await;
                    if !session.dirty {
                        None
                    } else {
                        session.dirty = false;
                        Some(session.strokes.clone())
                    }
                };
                if let Some(strokes) = maybe_strokes {
                    save_session(&backup_state.session_dir, &session_id, &strokes).await;
                }
            }
        }
    });

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("Whiteboard running at http://localhost:{port}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind server");
    axum::serve(listener, app).await.expect("Server crashed");
}

fn parse_path_arg(args: &[String], name: &str) -> Option<PathBuf> {
    let value = args
        .iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1));
    value.map(PathBuf::from)
}
