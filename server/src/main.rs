use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use pfboard_shared::{ClientMessage, Point, ServerMessage, Stroke};
use tokio::sync::{mpsc, RwLock};
use tower_http::services::ServeDir;
use uuid::Uuid;

const MAX_STROKES: usize = 2000;
const MAX_POINTS_PER_STROKE: usize = 5000;

#[derive(Clone)]
struct AppState {
    strokes: Arc<RwLock<Vec<Stroke>>>,
    active_ids: Arc<RwLock<HashSet<String>>>,
    peers: Arc<RwLock<HashMap<Uuid, mpsc::UnboundedSender<ServerMessage>>>>,
}

#[tokio::main]
async fn main() {
    let state = AppState {
        strokes: Arc::new(RwLock::new(Vec::new())),
        active_ids: Arc::new(RwLock::new(HashSet::new())),
        peers: Arc::new(RwLock::new(HashMap::new())),
    };

    let public_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../public");

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback_service(ServeDir::new(public_dir).append_index_html_on_directories(true))
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

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut socket_sender, mut socket_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();
    let connection_id = Uuid::new_v4();

    state.peers.write().await.insert(connection_id, tx);

    let strokes_snapshot = state.strokes.read().await.clone();
    if let Ok(sync_payload) = serde_json::to_string(&ServerMessage::Sync {
        strokes: strokes_snapshot,
    }) {
        let _ = socket_sender.send(Message::Text(sync_payload)).await;
    }

    let send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if let Ok(payload) = serde_json::to_string(&message) {
                if socket_sender.send(Message::Text(payload)).await.is_err() {
                    break;
                }
            }
        }
    });

    while let Some(Ok(message)) = socket_receiver.next().await {
        match message {
            Message::Text(text) => {
                let parsed = serde_json::from_str::<ClientMessage>(&text);
                if let Ok(client_message) = parsed {
                    if let Some(server_message) =
                        apply_client_message(&state, client_message).await
                    {
                        broadcast(&state, connection_id, server_message).await;
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    state.peers.write().await.remove(&connection_id);
    send_task.abort();
}

async fn apply_client_message(
    state: &AppState,
    message: ClientMessage,
) -> Option<ServerMessage> {
    match message {
        ClientMessage::StrokeStart {
            id,
            color,
            size,
            point,
        } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            let point = normalize_point(point)?;
            let color = sanitize_color(color);
            let size = sanitize_size(size);
            let stroke = Stroke {
                id: id.clone(),
                color: color.clone(),
                size,
                points: vec![point],
            };

            let removed = {
                let mut strokes = state.strokes.write().await;
                strokes.push(stroke);
                let overflow = strokes.len().saturating_sub(MAX_STROKES);
                if overflow > 0 {
                    strokes.drain(0..overflow).collect::<Vec<_>>()
                } else {
                    Vec::new()
                }
            };

            if !removed.is_empty() {
                let mut active = state.active_ids.write().await;
                for stroke in removed {
                    active.remove(&stroke.id);
                }
            }

            state.active_ids.write().await.insert(id.clone());

            Some(ServerMessage::StrokeStart {
                id,
                color,
                size,
                point,
            })
        }
        ClientMessage::StrokeMove { id, point } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            let point = normalize_point(point)?;
            if !state.active_ids.read().await.contains(&id) {
                return None;
            }

            let mut appended = false;
            {
                let mut strokes = state.strokes.write().await;
                if let Some(stroke) = strokes.iter_mut().find(|stroke| stroke.id == id) {
                    if stroke.points.len() < MAX_POINTS_PER_STROKE {
                        stroke.points.push(point);
                        appended = true;
                    }
                }
            }

            if appended {
                Some(ServerMessage::StrokeMove { id, point })
            } else {
                None
            }
        }
        ClientMessage::StrokeEnd { id } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            state.active_ids.write().await.remove(&id);
            Some(ServerMessage::StrokeEnd { id })
        }
        ClientMessage::Clear => {
            state.strokes.write().await.clear();
            state.active_ids.write().await.clear();
            Some(ServerMessage::Clear)
        }
    }
}

async fn broadcast(state: &AppState, sender: Uuid, message: ServerMessage) {
    let mut stale = Vec::new();
    {
        let peers = state.peers.read().await;
        for (id, tx) in peers.iter() {
            if *id == sender {
                continue;
            }
            if tx.send(message.clone()).is_err() {
                stale.push(*id);
            }
        }
    }

    if !stale.is_empty() {
        let mut peers = state.peers.write().await;
        for id in stale {
            peers.remove(&id);
        }
    }
}

fn normalize_point(point: Point) -> Option<Point> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return None;
    }
    Some(point.clamp())
}

fn sanitize_color(mut color: String) -> String {
    if color.is_empty() {
        return "#1f1f1f".to_string();
    }
    if color.len() > 32 {
        color.truncate(32);
    }
    color
}

fn sanitize_size(size: f32) -> f32 {
    let size = if size.is_finite() { size } else { 6.0 };
    size.max(1.0).min(60.0)
}
