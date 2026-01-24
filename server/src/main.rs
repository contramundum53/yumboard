use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
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
    sessions: Arc<RwLock<HashMap<String, Arc<Session>>>>,
    session_dir: PathBuf,
}

struct Session {
    strokes: Arc<RwLock<Vec<Stroke>>>,
    active_ids: Arc<RwLock<HashSet<String>>>,
    owners: Arc<RwLock<HashMap<String, Uuid>>>,
    histories: Arc<RwLock<HashMap<Uuid, ClientHistory>>>,
    peers: Arc<RwLock<HashMap<Uuid, mpsc::UnboundedSender<ServerMessage>>>>,
}

#[derive(Default)]
struct ClientHistory {
    undo: Vec<Action>,
    redo: Vec<Action>,
}

enum Action {
    AddStroke(Stroke),
    EraseStroke(Stroke),
    Clear { strokes: Vec<Stroke> },
    ReplaceStroke { before: Stroke, after: Stroke },
}

#[tokio::main]
async fn main() {
    let session_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sessions");
    if let Err(error) = tokio::fs::create_dir_all(&session_dir).await {
        eprintln!("Failed to create session dir: {error}");
    }
    let state = AppState {
        sessions: Arc::new(RwLock::new(HashMap::new())),
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

async fn root_handler(State(state): State<AppState>) -> impl IntoResponse {
    let session_id = new_session_id();
    let _ = get_or_create_session(&state, &session_id).await;
    Redirect::to(&format!("/s/{session_id}"))
}

async fn session_handler(
    Path(session_id): Path<String>,
    State(state): State<AppState>,
    axum::Extension(index_file): axum::Extension<PathBuf>,
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

async fn ws_handler(
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
    session.peers.write().await.insert(connection_id, tx);
    session
        .histories
        .write()
        .await
        .insert(connection_id, ClientHistory::default());

    let strokes_snapshot = session.strokes.read().await.clone();
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
                    if let Some((server_messages, include_sender)) = apply_client_message(
                        &session,
                        connection_id,
                        client_message,
                    )
                    .await
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

    session.peers.write().await.remove(&connection_id);
    session.histories.write().await.remove(&connection_id);
    send_task.abort();

    if session.peers.read().await.is_empty() {
        let strokes = session.strokes.read().await.clone();
        save_session(&state.session_dir, &session_id, &strokes).await;
        let mut sessions = state.sessions.write().await;
        if let Some(current) = sessions.get(&session_id) {
            if Arc::ptr_eq(current, &session) {
                sessions.remove(&session_id);
            }
        }
    }
}

async fn apply_client_message(
    session: &Session,
    sender: Uuid,
    message: ClientMessage,
) -> Option<(Vec<ServerMessage>, bool)> {
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
                let mut strokes = session.strokes.write().await;
                strokes.push(stroke);
                let overflow = strokes.len().saturating_sub(MAX_STROKES);
                if overflow > 0 {
                    strokes.drain(0..overflow).collect::<Vec<_>>()
                } else {
                    Vec::new()
                }
            };

            if !removed.is_empty() {
                let mut active = session.active_ids.write().await;
                let mut owners = session.owners.write().await;
                for stroke in removed {
                    active.remove(&stroke.id);
                    owners.remove(&stroke.id);
                }
            }

            session.active_ids.write().await.insert(id.clone());
            session.owners.write().await.insert(id.clone(), sender);

            Some((
                vec![ServerMessage::StrokeStart {
                    id,
                    color,
                    size,
                    point,
                }],
                false,
            ))
        }
        ClientMessage::StrokeMove { id, point } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            let point = normalize_point(point)?;
            if !session.active_ids.read().await.contains(&id) {
                return None;
            }

            let mut appended = false;
            {
                let mut strokes = session.strokes.write().await;
                if let Some(stroke) = strokes.iter_mut().find(|stroke| stroke.id == id) {
                    if stroke.points.len() < MAX_POINTS_PER_STROKE {
                        stroke.points.push(point);
                        appended = true;
                    }
                }
            }

            if appended {
                Some((vec![ServerMessage::StrokeMove { id, point }], false))
            } else {
                None
            }
        }
        ClientMessage::StrokeEnd { id } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }
            session.active_ids.write().await.remove(&id);
            if let Some(owner) = session.owners.read().await.get(&id) {
                if *owner == sender {
                    let stroke = {
                        let strokes = session.strokes.read().await;
                        strokes.iter().find(|stroke| stroke.id == id).cloned()
                    };
                    if let Some(stroke) = stroke {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.undo.push(Action::AddStroke(stroke));
                            history.redo.clear();
                        }
                    }
                }
            }
            Some((vec![ServerMessage::StrokeEnd { id }], false))
        }
        ClientMessage::Clear => {
            let cleared = session.strokes.write().await.drain(..).collect::<Vec<_>>();
            session.active_ids.write().await.clear();
            session.owners.write().await.clear();
            let mut histories = session.histories.write().await;
            if let Some(history) = histories.get_mut(&sender) {
                history.undo.push(Action::Clear { strokes: cleared });
                history.redo.clear();
            }
            Some((vec![ServerMessage::Clear], false))
        }
        ClientMessage::Undo => {
            let action = {
                let mut histories = session.histories.write().await;
                histories.get_mut(&sender).and_then(|history| history.undo.pop())
            }?;

            match action {
                Action::AddStroke(stroke) => {
                    let stroke_id = stroke.id.clone();
                    if remove_stroke(session, &stroke_id).await {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.redo.push(Action::AddStroke(stroke));
                        }
                        Some((vec![ServerMessage::StrokeRemove { id: stroke_id }], true))
                    } else {
                        None
                    }
                }
                Action::EraseStroke(stroke) => {
                    add_stroke(session, stroke.clone(), Some(sender)).await;
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.redo.push(Action::EraseStroke(stroke.clone()));
                    }
                    Some((vec![ServerMessage::StrokeRestore { stroke }], true))
                }
                Action::Clear { strokes } => {
                    for stroke in &strokes {
                        add_stroke(session, stroke.clone(), None).await;
                    }
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.redo.push(Action::Clear {
                            strokes: strokes.clone(),
                        });
                    }
                    let messages = strokes
                        .into_iter()
                        .map(|stroke| ServerMessage::StrokeRestore { stroke })
                        .collect::<Vec<_>>();
                    Some((messages, true))
                }
                Action::ReplaceStroke { before, after } => {
                    let replaced = replace_stroke(session, before.clone()).await;
                    if replaced.is_some() {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.redo.push(Action::ReplaceStroke {
                                before: before.clone(),
                                after,
                            });
                        }
                        Some((vec![ServerMessage::StrokeReplace { stroke: before }], true))
                    } else {
                        None
                    }
                }
            }
        }
        ClientMessage::Redo => {
            let action = {
                let mut histories = session.histories.write().await;
                histories.get_mut(&sender).and_then(|history| history.redo.pop())
            }?;

            match action {
                Action::AddStroke(stroke) => {
                    add_stroke(session, stroke.clone(), Some(sender)).await;
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.undo.push(Action::AddStroke(stroke.clone()));
                    }
                    Some((vec![ServerMessage::StrokeRestore { stroke }], true))
                }
                Action::EraseStroke(stroke) => {
                    let stroke_id = stroke.id.clone();
                    if remove_stroke(session, &stroke_id).await {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.undo.push(Action::EraseStroke(stroke));
                        }
                        Some((vec![ServerMessage::StrokeRemove { id: stroke_id }], true))
                    } else {
                        None
                    }
                }
                Action::Clear { strokes } => {
                    session.strokes.write().await.clear();
                    session.active_ids.write().await.clear();
                    session.owners.write().await.clear();
                    let mut histories = session.histories.write().await;
                    if let Some(history) = histories.get_mut(&sender) {
                        history.undo.push(Action::Clear { strokes });
                    }
                    Some((vec![ServerMessage::Clear], true))
                }
                Action::ReplaceStroke { before, after } => {
                    let replaced = replace_stroke(session, after.clone()).await;
                    if replaced.is_some() {
                        let mut histories = session.histories.write().await;
                        if let Some(history) = histories.get_mut(&sender) {
                            history.undo.push(Action::ReplaceStroke {
                                before,
                                after: after.clone(),
                            });
                        }
                        Some((vec![ServerMessage::StrokeReplace { stroke: after }], true))
                    } else {
                        None
                    }
                }
            }
        }
        ClientMessage::Erase { id } => {
            if id.is_empty() || id.len() > 64 {
                return None;
            }

            let removed = {
                let mut strokes = session.strokes.write().await;
                if let Some(index) = strokes.iter().position(|s| s.id == id) {
                    Some(strokes.remove(index))
                } else {
                    None
                }
            };

            if let Some(stroke) = removed {
                session.active_ids.write().await.remove(&id);
                session.owners.write().await.remove(&id);
                let mut histories = session.histories.write().await;
                if let Some(history) = histories.get_mut(&sender) {
                    history.undo.push(Action::EraseStroke(stroke));
                    history.redo.clear();
                }
                Some((vec![ServerMessage::StrokeRemove { id }], true))
            } else {
                None
            }
        }
        ClientMessage::StrokeReplace { stroke } => {
            let stroke = sanitize_stroke(stroke)?;
            let before = replace_stroke(session, stroke.clone()).await?;
            let mut histories = session.histories.write().await;
            if let Some(history) = histories.get_mut(&sender) {
                history
                    .undo
                    .push(Action::ReplaceStroke { before, after: stroke.clone() });
                history.redo.clear();
            }
            Some((vec![ServerMessage::StrokeReplace { stroke }], false))
        }
        ClientMessage::Remove { ids } => {
            if ids.is_empty() {
                return None;
            }
            let mut removed = Vec::new();
            for id in ids {
                if id.is_empty() || id.len() > 64 {
                    continue;
                }
                let stroke = remove_stroke_full(session, &id).await;
                if let Some(stroke) = stroke {
                    removed.push(stroke);
                }
            }
            if removed.is_empty() {
                return None;
            }
            let mut histories = session.histories.write().await;
            if let Some(history) = histories.get_mut(&sender) {
                for stroke in &removed {
                    history.undo.push(Action::EraseStroke(stroke.clone()));
                }
                history.redo.clear();
            }
            let messages = removed
                .into_iter()
                .map(|stroke| ServerMessage::StrokeRemove { id: stroke.id })
                .collect::<Vec<_>>();
            Some((messages, false))
        }
        ClientMessage::Load { strokes } => {
            let strokes = sanitize_strokes(strokes);
            {
                let mut stored = session.strokes.write().await;
                *stored = strokes.clone();
            }
            session.active_ids.write().await.clear();
            session.owners.write().await.clear();
            let mut histories = session.histories.write().await;
            for history in histories.values_mut() {
                history.undo.clear();
                history.redo.clear();
            }
            Some((vec![ServerMessage::Sync { strokes }], true))
        }
    }
}

async fn broadcast_except(session: &Session, sender: Uuid, message: ServerMessage) {
    let mut stale = Vec::new();
    {
        let peers = session.peers.read().await;
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
        let mut peers = session.peers.write().await;
        for id in stale {
            peers.remove(&id);
        }
    }
}

async fn broadcast_all(session: &Session, message: ServerMessage) {
    let mut stale = Vec::new();
    {
        let peers = session.peers.read().await;
        for (id, tx) in peers.iter() {
            if tx.send(message.clone()).is_err() {
                stale.push(*id);
            }
        }
    }

    if !stale.is_empty() {
        let mut peers = session.peers.write().await;
        for id in stale {
            peers.remove(&id);
        }
    }
}

async fn remove_stroke(session: &Session, id: &str) -> bool {
    let removed = {
        let mut strokes = session.strokes.write().await;
        if let Some(index) = strokes.iter().position(|s| s.id == id) {
            strokes.remove(index);
            true
        } else {
            false
        }
    };
    if removed {
        session.active_ids.write().await.remove(id);
        session.owners.write().await.remove(id);
    }
    removed
}

async fn add_stroke(session: &Session, stroke: Stroke, owner: Option<Uuid>) {
    let removed = {
        let mut strokes = session.strokes.write().await;
        strokes.push(stroke.clone());
        let overflow = strokes.len().saturating_sub(MAX_STROKES);
        if overflow > 0 {
            strokes.drain(0..overflow).collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    };

    if !removed.is_empty() {
        let mut active = session.active_ids.write().await;
        let mut owners = session.owners.write().await;
        for stroke in removed {
            active.remove(&stroke.id);
            owners.remove(&stroke.id);
        }
    }

    if let Some(owner) = owner {
        session.owners.write().await.insert(stroke.id.clone(), owner);
    }
}

fn normalize_point(point: Point) -> Option<Point> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return None;
    }
    Some(point)
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

fn sanitize_stroke(mut stroke: Stroke) -> Option<Stroke> {
    if stroke.id.is_empty() || stroke.id.len() > 64 {
        return None;
    }
    stroke.color = sanitize_color(stroke.color);
    stroke.size = sanitize_size(stroke.size);
    stroke.points = stroke
        .points
        .into_iter()
        .filter_map(normalize_point)
        .collect();
    if stroke.points.is_empty() {
        return None;
    }
    Some(stroke)
}

fn sanitize_strokes(strokes: Vec<Stroke>) -> Vec<Stroke> {
    strokes
        .into_iter()
        .filter_map(sanitize_stroke)
        .collect()
}

async fn replace_stroke(session: &Session, stroke: Stroke) -> Option<Stroke> {
    let mut strokes = session.strokes.write().await;
    if let Some(index) = strokes.iter().position(|s| s.id == stroke.id) {
        let before = strokes[index].clone();
        strokes[index] = stroke;
        Some(before)
    } else {
        None
    }
}

async fn remove_stroke_full(session: &Session, id: &str) -> Option<Stroke> {
    let removed = {
        let mut strokes = session.strokes.write().await;
        if let Some(index) = strokes.iter().position(|s| s.id == id) {
            Some(strokes.remove(index))
        } else {
            None
        }
    };
    if removed.is_some() {
        session.active_ids.write().await.remove(id);
        session.owners.write().await.remove(id);
    }
    removed
}

impl Session {
    fn new(strokes: Vec<Stroke>) -> Self {
        Self {
            strokes: Arc::new(RwLock::new(strokes)),
            active_ids: Arc::new(RwLock::new(HashSet::new())),
            owners: Arc::new(RwLock::new(HashMap::new())),
            histories: Arc::new(RwLock::new(HashMap::new())),
            peers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

fn new_session_id() -> String {
    Uuid::new_v4().to_string()
}

fn normalize_session_id(value: &str) -> Option<String> {
    let parsed = Uuid::parse_str(value).ok()?;
    Some(parsed.to_string())
}

async fn get_or_create_session(state: &AppState, session_id: &str) -> Arc<Session> {
    if let Some(session) = state.sessions.read().await.get(session_id).cloned() {
        return session;
    }
    let strokes = load_session(&state.session_dir, session_id).await.unwrap_or_default();
    let session = Arc::new(Session::new(strokes));
    let mut sessions = state.sessions.write().await;
    let entry = sessions
        .entry(session_id.to_string())
        .or_insert_with(|| session.clone());
    entry.clone()
}

async fn load_session(session_dir: &PathBuf, session_id: &str) -> Option<Vec<Stroke>> {
    let path = session_dir.join(format!("{session_id}.json"));
    let payload = tokio::fs::read_to_string(path).await.ok()?;
    let strokes = serde_json::from_str::<Vec<Stroke>>(&payload).ok()?;
    Some(sanitize_strokes(strokes))
}

async fn save_session(session_dir: &PathBuf, session_id: &str, strokes: &[Stroke]) {
    let path = session_dir.join(format!("{session_id}.json"));
    if let Ok(payload) = serde_json::to_string(strokes) {
        if let Err(error) = tokio::fs::write(path, payload).await {
            eprintln!("Failed to save session {session_id}: {error}");
        }
    }
}
