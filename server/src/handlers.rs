use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use uuid::Uuid;
use yumboard_shared::{ClientMessage, ServerMessage};

use crate::logic::{apply_client_message, broadcast_all, broadcast_except};
use crate::sessions::{get_or_create_session, new_session_id, normalize_session_id, save_session};
use crate::state::AppState;

pub async fn ping_handler() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

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
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
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
        eprintln!(
            "WS connected session={session_id} conn={connection_id} peers={}",
            session.peers.len()
        );
    }

    let strokes_snapshot = session.read().await.strokes.clone();
    let strokes_len = strokes_snapshot.len();
    if let Ok(sync_payload) = bincode::encode_to_vec(
        &ServerMessage::Sync {
            strokes: strokes_snapshot,
        },
        bincode::config::standard(),
    ) {
        eprintln!(
            "WS sync send session={session_id} conn={connection_id} strokes={strokes_len} bytes={}",
            sync_payload.len()
        );
        if let Err(error) = socket_sender.send(Message::Binary(sync_payload)).await {
            eprintln!(
                "WS sync send failed session={session_id} conn={connection_id} error={error:?}"
            );
        }
    } else {
        eprintln!("WS sync serialize failed session={session_id} conn={connection_id}");
    }

    let send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if let Ok(payload) = bincode::encode_to_vec(&message, bincode::config::standard()) {
                if socket_sender.send(Message::Binary(payload)).await.is_err() {
                    break;
                }
            }
        }
    });

    let mut close_frame = None;

    while let Some(Ok(message)) = socket_receiver.next().await {
        match message {
            Message::Text(text) => {
                let parsed = serde_json::from_str::<ClientMessage>(&text);
                if let Ok(client_message) = parsed {
                    let result = {
                        let mut session_guard = session.write().await;
                        apply_client_message(&mut session_guard, connection_id, client_message)
                    };
                    if let Some((server_messages, include_sender)) = result {
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
            Message::Binary(data) => {
                let parsed = bincode::decode_from_slice::<ClientMessage, _>(
                    &data,
                    bincode::config::standard(),
                );
                if let Ok((client_message, _)) = parsed {
                    let result = {
                        let mut session_guard = session.write().await;
                        apply_client_message(&mut session_guard, connection_id, client_message)
                    };
                    if let Some((server_messages, include_sender)) = result {
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
            Message::Close(frame) => {
                close_frame = frame;
                break;
            }
            _ => {}
        }
    }

    {
        let mut session = session.write().await;
        session.peers.remove(&connection_id);
        session.histories.remove(&connection_id);
        session.transform_sessions.remove(&connection_id);
        eprintln!(
            "WS disconnected session={session_id} conn={connection_id} peers={}",
            session.peers.len()
        );
        if let Some(frame) = &close_frame {
            eprintln!(
                "WS close frame session={session_id} conn={connection_id} code={:?} reason={:?}",
                frame.code, frame.reason
            );
        }
    }
    send_task.abort();

    let mut should_remove = false;
    let mut maybe_data = None;
    {
        let session_guard = session.read().await;
        if session_guard.peers.is_empty() {
            should_remove = true;
            if session_guard.dirty {
                maybe_data = Some(session_guard.to_persistent_session_data());
            }
        }
    }
    if let Some(data) = maybe_data {
        eprint!("Saving finished session {session_id}... ");
        save_session(&state, &session_id, &data).await;
        eprintln!("done.");
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
