use std::sync::Arc;

use uuid::Uuid;
use yumboard_shared::Stroke;

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
    let strokes = state
        .storage
        .load_session(session_id)
        .await
        .unwrap_or_default();
    let session = Arc::new(tokio::sync::RwLock::new(Session::new(sanitize_strokes(
        strokes,
    ))));
    let mut sessions = state.sessions.write().await;
    let entry = sessions
        .entry(session_id.to_string())
        .or_insert_with(|| session.clone());
    entry.clone()
}

pub async fn save_session(state: &AppState, session_id: &str, strokes: &[Stroke]) {
    state.storage.save_session(session_id, strokes).await;
}
