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
    owners: Arc<RwLock<HashMap<String, Uuid>>>,
    histories: Arc<RwLock<HashMap<Uuid, ClientHistory>>>,
    peers: Arc<RwLock<HashMap<Uuid, mpsc::UnboundedSender<ServerMessage>>>>,
}

#[derive(Default)]
struct ClientHistory {
    undo: Vec<Stroke>,
    redo: Vec<Stroke>,
}

#[tokio::main]
async fn main() {
    let state = AppState {
        strokes: Arc::new(RwLock::new(Vec::new())),
        active_ids: Arc::new(RwLock::new(HashSet::new())),
        owners: Arc::new(RwLock::new(HashMap::new())),
        histories: Arc::new(RwLock::new(HashMap::new())),
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
    state
        .histories
        .write()
        .await
        .insert(connection_id, ClientHistory::default());

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
                    if let Some((server_message, include_sender)) =
                        apply_client_message(&state, connection_id, client_message).await
                    {
                        if include_sender {
                            broadcast_all(&state, server_message).await;
                        } else {
                            broadcast_except(&state, connection_id, server_message).await;
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    state.peers.write().await.remove(&connection_id);
    state.histories.write().await.remove(&connection_id);
    send_task.abort();
}

async fn apply_client_message(
    state: &AppState,
    sender: Uuid,
    message: ClientMessage,
) -> Option<(ServerMessage, bool)> {
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
                let mut owners = state.owners.write().await;
                for stroke in removed {
                    active.remove(&stroke.id);
                    owners.remove(&stroke.id);
                }
            }

            state.active_ids.write().await.insert(id.clone());
            state.owners.write().await.insert(id.clone(), sender);

            Some((
                ServerMessage::StrokeStart {
                    id,
                    color,
                    size,
                    point,
                },
                false,
            ))
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
                Some((ServerMessage::StrokeMove { id, point }, false))
            } else {
                None
            }
        }
        ClientMessage::StrokeEnd { id } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            state.active_ids.write().await.remove(&id);
            if let Some(owner) = state.owners.read().await.get(&id) {
                if *owner == sender {
                    let stroke = {
                        let strokes = state.strokes.read().await;
                        strokes.iter().find(|stroke| stroke.id == id).cloned()
                    };
                    if let Some(stroke) = stroke {
                        let mut histories = state.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.undo.push(stroke);
                            history.redo.clear();
                        }
                    }
                }
            }
            Some((ServerMessage::StrokeEnd { id }, false))
        }
        ClientMessage::Clear => {
            state.strokes.write().await.clear();
            state.active_ids.write().await.clear();
            state.owners.write().await.clear();
            let mut histories = state.histories.write().await;
            for history in histories.values_mut() {
                history.undo.clear();
                history.redo.clear();
            }
            Some((ServerMessage::Clear, false))
        }
        ClientMessage::Undo => {
            let stroke = {
                let mut histories = state.histories.write().await;
                histories.get_mut(&sender).and_then(|history| history.undo.pop())
            }?;
            let stroke_id = stroke.id.clone();

            let removed = {
                let mut strokes = state.strokes.write().await;
                if let Some(index) = strokes.iter().position(|s| s.id == stroke_id) {
                    strokes.remove(index);
                    true
                } else {
                    false
                }
            };

            if removed {
                state.active_ids.write().await.remove(&stroke_id);
                let mut histories = state.histories.write().await;
                if let Some(history) = histories.get_mut(&sender) {
                    history.redo.push(stroke);
                }
                Some((ServerMessage::StrokeRemove { id: stroke_id }, true))
            } else {
                None
            }
        }
        ClientMessage::Redo => {
            let stroke = {
                let mut histories = state.histories.write().await;
                histories.get_mut(&sender).and_then(|history| history.redo.pop())
            }?;

            {
                let mut strokes = state.strokes.write().await;
                strokes.push(stroke.clone());
            }

            let mut histories = state.histories.write().await;
            if let Some(history) = histories.get_mut(&sender) {
                history.undo.push(stroke.clone());
            }

            Some((ServerMessage::StrokeRestore { stroke }, true))
        }
    }
}

async fn broadcast_except(state: &AppState, sender: Uuid, message: ServerMessage) {
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

async fn broadcast_all(state: &AppState, message: ServerMessage) {
    let mut stale = Vec::new();
    {
        let peers = state.peers.read().await;
        for (id, tx) in peers.iter() {
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
