use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::http::header::{CACHE_CONTROL, EXPIRES, PRAGMA};
use axum::http::HeaderValue;
use axum::routing::{any, get};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

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

    // PEM certificate for HTTPS (mkcert outputs .pem files)
    #[arg(long)]
    tls_cert: Option<PathBuf>,

    // PEM private key for HTTPS (mkcert outputs -key.pem files)
    #[arg(long)]
    tls_key: Option<PathBuf>,

    // Interval (in seconds) for periodic backups
    #[arg(long, default_value_t = 60u64)]
    backup_interval: u64,

    /// Disable HTTP/2 and serve only HTTP/1.1 (helps debug iOS Safari WebSocket issues).
    #[arg(long, default_value_t = false)]
    http1_only: bool,

    #[arg(long, default_value_t = 3000)]
    port: u16,
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
        .route("/ws/:session_id", any(ws_handler))
        .fallback_service(ServeDir::new(public_dir).append_index_html_on_directories(true))
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            PRAGMA,
            HeaderValue::from_static("no-cache"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            EXPIRES,
            HeaderValue::from_static("0"),
        ))
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

    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    let use_tls = args.tls_cert.is_some() || args.tls_key.is_some();
    if use_tls {
        let cert = args
            .tls_cert
            .expect("Missing --tls-cert (required when enabling TLS)");
        let key = args
            .tls_key
            .expect("Missing --tls-key (required when enabling TLS)");
        println!("Whiteboard running at https://localhost:{}", args.port);
        let config = RustlsConfig::from_pem_file(cert, key)
            .await
            .expect("Failed to load TLS certificate/key");
        let config = if args.http1_only {
            let mut server_config = (*config.get_inner()).clone();
            server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
            RustlsConfig::from_config(Arc::new(server_config))
        } else {
            config
        };
        let server = axum_server::bind_rustls(addr, config);
        let server = if args.http1_only {
            server.http1_only()
        } else {
            server
        };
        server
            .serve(app.into_make_service())
            .await
            .expect("Server crashed");
    } else {
        println!("Whiteboard running at http://localhost:{}", args.port);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("Failed to bind server");
        axum::serve(listener, app).await.expect("Server crashed");
    }
}
