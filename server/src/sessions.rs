use std::sync::Arc;

use crate::logic::sanitize_strokes;
use crate::state::{AppState, PersistentSessionData, Session};
use uuid::Uuid;

pub fn new_session_id() -> String {
    Uuid::now_v7().to_string()
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
    let res = state.storage.load_session(session_id).await;

    let data = match res {
        Ok(data) => data,
        Err(err) => {
            eprintln!("Could not load session {session_id}: {err}\nCreating new session.");
            PersistentSessionData::default()
        }
    };

    let sanitized = PersistentSessionData {
        strokes: sanitize_strokes(data.strokes),
    };
    let session = Arc::new(tokio::sync::RwLock::new(
        Session::from_persistent_session_data(sanitized),
    ));
    let mut sessions = state.sessions.write().await;
    let entry = sessions
        .entry(session_id.to_string())
        .or_insert_with(|| session.clone());
    entry.clone()
}

pub async fn save_session(state: &AppState, session_id: &str, data: &PersistentSessionData) {
    state.storage.save_session(session_id, data).await;
}
