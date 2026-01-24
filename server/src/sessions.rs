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

pub async fn get_or_create_session(state: &AppState, session_id: &str) -> Arc<Session> {
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

async fn load_session(session_dir: &std::path::PathBuf, session_id: &str) -> Option<Vec<Stroke>> {
    let path = session_dir.join(format!("{session_id}.json"));
    let payload = tokio::fs::read_to_string(path).await.ok()?;
    let strokes = serde_json::from_str::<Vec<Stroke>>(&payload).ok()?;
    Some(sanitize_strokes(strokes))
}

pub async fn save_session(session_dir: &std::path::PathBuf, session_id: &str, strokes: &[Stroke]) {
    let path = session_dir.join(format!("{session_id}.json"));
    if let Ok(payload) = serde_json::to_string(strokes) {
        if let Err(error) = tokio::fs::write(path, payload).await {
            eprintln!("Failed to save session {session_id}: {error}");
        }
    }
}

pub async fn save_session_backup(
    session_dir: &std::path::PathBuf,
    session_id: &str,
    strokes: &[Stroke],
) {
    let backup_dir = session_dir.join("backups");
    if let Err(error) = tokio::fs::create_dir_all(&backup_dir).await {
        eprintln!("Failed to create backup dir: {error}");
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0);
    let path = backup_dir.join(format!("{session_id}-{ts}.json"));
    if let Ok(payload) = serde_json::to_string(strokes) {
        if let Err(error) = tokio::fs::write(path, payload).await {
            eprintln!("Failed to backup session {session_id}: {error}");
        }
    }
}
