use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use futures_util::{SinkExt, StreamExt};
use pfboard_shared::{ClientMessage, ServerMessage};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::logic::{apply_client_message, broadcast_all, broadcast_except};
use crate::sessions::{get_or_create_session, new_session_id, normalize_session_id, save_session};
use crate::state::AppState;

pub async fn root_handler(State(state): State<AppState>) -> impl IntoResponse {
    let session_id = new_session_id();
    let _ = get_or_create_session(&state, &session_id).await;
    Redirect::to(&format!("/s/{session_id}"))
}

pub async fn session_handler(
    Path(session_id): Path<String>,
    State(state): State<AppState>,
    axum::Extension(index_file): axum::Extension<std::path::PathBuf>,
) -> impl IntoResponse {
    let session_id = match normalize_session_id(&session_id) {
        Some(id) => id,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let _ = get_or_create_session(&state, &session_id).await;
    match tokio::fs::read_to_string(index_file).await {
        Ok(contents) => Html(contents).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn ws_handler(
    Path(session_id): Path<String>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let session_id = match normalize_session_id(&session_id) {
        Some(id) => id,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    ws.on_upgrade(move |socket| handle_socket(socket, state, session_id))
}

async fn handle_socket(socket: WebSocket, state: AppState, session_id: String) {
    let (mut socket_sender, mut socket_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();
    let connection_id = Uuid::new_v4();

    let session = get_or_create_session(&state, &session_id).await;
    {
        let mut session = session.write().await;
        session.peers.insert(connection_id, tx);
        session
            .histories
            .insert(connection_id, crate::state::ClientHistory::default());
    }

    let strokes_snapshot = session.read().await.strokes.clone();
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
                    if let Some((server_messages, include_sender)) =
                        apply_client_message(&mut *session.write().await, connection_id, client_message)
                    {
                        for server_message in server_messages {
                            if include_sender {
                                broadcast_all(&session, server_message).await;
                            } else {
                                broadcast_except(&session, connection_id, server_message).await;
                            }
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    {
        let mut session = session.write().await;
        session.peers.remove(&connection_id);
        session.histories.remove(&connection_id);
        session.transform_sessions.remove(&connection_id);
    }
    send_task.abort();

    let mut should_remove = false;
    let mut maybe_strokes = None;
    {
        let mut session_guard = session.write().await;
        if session_guard.peers.is_empty() {
            should_remove = true;
            if session_guard.dirty {
                session_guard.dirty = false;
                maybe_strokes = Some(session_guard.strokes.clone());
            }
        }
    }
    if let Some(strokes) = maybe_strokes {
        save_session(&state.session_dir, &session_id, &strokes).await;
    }
    if should_remove {
        let mut sessions = state.sessions.write().await;
        if let Some(current) = sessions.get(&session_id) {
            if Arc::ptr_eq(current, &session) {
                sessions.remove(&session_id);
            }
        }
    }
}
