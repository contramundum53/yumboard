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
    let mut buf = Vec::new();
    write_u32(&mut buf, strokes.len() as u32);
    for stroke in strokes {
        write_string(&mut buf, &stroke.id);
        write_string(&mut buf, &stroke.color);
        write_f32(&mut buf, stroke.size);
        write_u32(&mut buf, stroke.points.len() as u32);
        for point in &stroke.points {
            write_f32(&mut buf, point.x);
            write_f32(&mut buf, point.y);
        }
    }
    buf
}

fn decode_strokes(payload: &[u8]) -> Option<Vec<Stroke>> {
    let mut offset = 0usize;
    let count = read_u32(payload, &mut offset)? as usize;
    let mut strokes = Vec::with_capacity(count);
    for _ in 0..count {
        let id = read_string(payload, &mut offset)?;
        let color = read_string(payload, &mut offset)?;
        let size = read_f32(payload, &mut offset)?;
        let points_len = read_u32(payload, &mut offset)? as usize;
        let mut points = Vec::with_capacity(points_len);
        for _ in 0..points_len {
            let x = read_f32(payload, &mut offset)?;
            let y = read_f32(payload, &mut offset)?;
            points.push(pfboard_shared::Point { x, y });
        }
        strokes.push(Stroke {
            id,
            color,
            size,
            points,
        });
    }
    Some(strokes)
}

fn write_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn write_f32(buf: &mut Vec<u8>, value: f32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn write_string(buf: &mut Vec<u8>, value: &str) {
    write_u32(buf, value.len() as u32);
    buf.extend_from_slice(value.as_bytes());
}

fn read_u32(payload: &[u8], offset: &mut usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let bytes = payload.get(*offset..end)?;
    *offset = end;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_f32(payload: &[u8], offset: &mut usize) -> Option<f32> {
    let end = offset.checked_add(4)?;
    let bytes = payload.get(*offset..end)?;
    *offset = end;
    Some(f32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_string(payload: &[u8], offset: &mut usize) -> Option<String> {
    let len = read_u32(payload, offset)? as usize;
    let end = offset.checked_add(len)?;
    let bytes = payload.get(*offset..end)?;
    *offset = end;
    std::str::from_utf8(bytes)
        .ok()
        .map(|value| value.to_string())
}
