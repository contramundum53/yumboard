use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use clap::Parser;
use tower_http::services::ServeDir;

mod handlers;
mod logic;
mod sessions;
mod state;

use crate::handlers::{root_handler, session_handler, ws_handler};
use crate::sessions::save_session;
use crate::state::AppState;

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    // Directory to store session data
    #[arg(long)]
    sessions_dir: Option<PathBuf>,

    // Directory to serve static files from
    #[arg(long)]
    public_dir: Option<PathBuf>,

    // Interval (in seconds) for periodic backups
    #[arg(long, default_value_t = 60u64)]
    backup_interval: u64,
}

async fn save_all_sessions(state: &AppState, reset_dirty: bool) {
    let sessions = {
        let sessions = state.sessions.read().await;
        sessions
            .iter()
            .map(|(session_id, session)| (session_id.clone(), session.clone()))
            .collect::<Vec<_>>()
    };
    for (session_id, session) in sessions {
        let strokes = {
            let session = session.read().await;
            if !session.dirty {
                continue;
            }
            session.strokes.clone()
        };
        eprint!("Saving session {session_id}... ");
        save_session(&state.session_dir, &session_id, &strokes).await;
        eprintln!("done.");
        if reset_dirty {
            session.write().await.dirty = false;
        }
    }
}

async fn periodic_backup_loop(state: AppState, interval_secs: u64) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        eprintln!("Starting periodic backup...");
        save_all_sessions(&state, true).await;
        eprintln!("Periodic backup completed.");
    }
}

async fn shutdown_signal(app_state: AppState) {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");

    tokio::select! {
        val = ctrl_c => {
            val.expect("Failed to listen for Ctrl-C");
            eprintln!("Received Ctrl-C, saving all sessions...");
        }
        val = sigterm.recv() => {
            val.expect("Failed to listen for SIGTERM");
            eprintln!("Received SIGTERM, saving all sessions...");
        }
    }
    save_all_sessions(&app_state, false).await;
    eprintln!("All sessions saved. Shutting down.");
    std::process::exit(0);
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let session_dir = args
        .sessions_dir
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sessions"));
    if let Err(error) = tokio::fs::create_dir_all(&session_dir).await {
        eprintln!("Failed to create session dir: {error}");
    }
    let public_dir = args
        .public_dir
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../public"));
    let index_file = public_dir.join("index.html");

    let backup_interval = args.backup_interval;

    let state = AppState {
        sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        session_dir,
    };

    let backup_state = state.clone();
    let backup_state2 = state.clone();

    let app = Router::new()
        .route("/", get(root_handler))
        .route("/s/:session_id", get(session_handler))
        .route("/ws/:session_id", get(ws_handler))
        .fallback_service(ServeDir::new(public_dir).append_index_html_on_directories(true))
        .layer(axum::Extension(index_file))
        .with_state(state);

    // Periodic backup loop
    tokio::spawn(async move {
        periodic_backup_loop(backup_state, backup_interval).await;
    });

    // Shutdown signal handler
    tokio::spawn(async move {
        shutdown_signal(backup_state2).await;
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
