use std::path::PathBuf;

use crate::state::PersistentSessionData;
use async_trait::async_trait;

#[async_trait]
pub trait Storage: Send + Sync {
    async fn load_session(&self, session_id: &str) -> Option<PersistentSessionData>;
    async fn save_session(&self, session_id: &str, data: &PersistentSessionData);
}

pub struct FileStorage {
    session_dir: PathBuf,
}

impl FileStorage {
    pub fn new(session_dir: PathBuf) -> Self {
        Self { session_dir }
    }
}

#[async_trait]
impl Storage for FileStorage {
    async fn load_session(&self, session_id: &str) -> Option<PersistentSessionData> {
        let path = self.session_dir.join(format!("{session_id}.bin"));
        let payload = tokio::fs::read(path).await.ok()?;
        decode_data(&payload)
    }

    async fn save_session(&self, session_id: &str, data: &PersistentSessionData) {
        let path = self.session_dir.join(format!("{session_id}.bin"));
        let payload = encode_data(data);
        if let Err(error) = tokio::fs::write(path, payload).await {
            eprintln!("Failed to save session {session_id}: {error}");
        }
    }
}

fn encode_data(data: &PersistentSessionData) -> Vec<u8> {
    bincode::encode_to_vec(data, bincode::config::standard()).unwrap_or_default()
}

fn decode_data(payload: &[u8]) -> Option<PersistentSessionData> {
    bincode::decode_from_slice(payload, bincode::config::standard())
        .map(|(data, _)| data)
        .ok()
}
