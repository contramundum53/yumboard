use std::path::PathBuf;

use async_trait::async_trait;
use yumboard_shared::Stroke;

#[async_trait]
pub trait Storage: Send + Sync {
    async fn load_session(&self, session_id: &str) -> Option<Vec<Stroke>>;
    async fn save_session(&self, session_id: &str, strokes: &[Stroke]);
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
    async fn load_session(&self, session_id: &str) -> Option<Vec<Stroke>> {
        let path = self.session_dir.join(format!("{session_id}.bin"));
        let payload = tokio::fs::read(path).await.ok()?;
        decode_strokes(&payload)
    }

    async fn save_session(&self, session_id: &str, strokes: &[Stroke]) {
        let path = self.session_dir.join(format!("{session_id}.bin"));
        let payload = encode_strokes(strokes);
        if let Err(error) = tokio::fs::write(path, payload).await {
            eprintln!("Failed to save session {session_id}: {error}");
        }
    }
}

fn encode_strokes(strokes: &[Stroke]) -> Vec<u8> {
    bincode::encode_to_vec(strokes, bincode::config::standard()).unwrap_or_default()
}

fn decode_strokes(payload: &[u8]) -> Option<Vec<Stroke>> {
    bincode::decode_from_slice(payload, bincode::config::standard())
        .map(|(strokes, _)| strokes)
        .ok()
}
