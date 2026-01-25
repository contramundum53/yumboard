use std::sync::Arc;

use pfboard_shared::Stroke;
use uuid::Uuid;

use crate::logic::sanitize_strokes;
use crate::state::{AppState, Session};

pub fn new_session_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn normalize_session_id(value: &str) -> Option<String> {
    let parsed = Uuid::parse_str(value).ok()?;
    Some(parsed.to_string())
}

pub async fn get_or_create_session(
    state: &AppState,
    session_id: &str,
) -> Arc<tokio::sync::RwLock<Session>> {
    if let Some(session) = state.sessions.read().await.get(session_id).cloned() {
        return session;
    }
    eprintln!("Loading/Creating session {session_id}...");
    let strokes = load_session(&state.session_dir, session_id)
        .await
        .unwrap_or_default();
    let session = Arc::new(tokio::sync::RwLock::new(Session::new(strokes)));
    let mut sessions = state.sessions.write().await;
    let entry = sessions
        .entry(session_id.to_string())
        .or_insert_with(|| session.clone());
    entry.clone()
}

async fn load_session(session_dir: &std::path::PathBuf, session_id: &str) -> Option<Vec<Stroke>> {
    let path = session_dir.join(format!("{session_id}.bin"));
    let payload = tokio::fs::read(path).await.ok()?;
    let strokes = decode_strokes(&payload)?;
    Some(sanitize_strokes(strokes))
}

pub async fn save_session(session_dir: &std::path::PathBuf, session_id: &str, strokes: &[Stroke]) {
    let path = session_dir.join(format!("{session_id}.bin"));
    let payload = encode_strokes(strokes);
    if let Err(error) = tokio::fs::write(path, payload).await {
        eprintln!("Failed to save session {session_id}: {error}");
    }
}

fn encode_strokes(strokes: &[Stroke]) -> Vec<u8> {
    bincode::serialize(strokes).unwrap_or_default()
}

fn decode_strokes(payload: &[u8]) -> Option<Vec<Stroke>> {
    bincode::deserialize(payload).ok()
}
