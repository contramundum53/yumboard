use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use pfboard_shared::Stroke;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

pub const MAX_STROKES: usize = 2000;
pub const MAX_POINTS_PER_STROKE: usize = 5000;

#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<RwLock<HashMap<String, Arc<Session>>>>,
    pub session_dir: PathBuf,
}

pub struct Session {
    pub strokes: Arc<RwLock<Vec<Stroke>>>,
    pub active_ids: Arc<RwLock<HashSet<String>>>,
    pub owners: Arc<RwLock<HashMap<String, Uuid>>>,
    pub histories: Arc<RwLock<HashMap<Uuid, ClientHistory>>>,
    pub peers: Arc<RwLock<HashMap<Uuid, mpsc::UnboundedSender<pfboard_shared::ServerMessage>>>>,
    pub transform_sessions: Arc<RwLock<HashMap<Uuid, TransformSession>>>,
}

#[derive(Default)]
pub struct ClientHistory {
    pub undo: Vec<Action>,
    pub redo: Vec<Action>,
}

pub enum Action {
    AddStroke(Stroke),
    EraseStroke(Stroke),
    Clear { strokes: Vec<Stroke> },
    ReplaceStroke { before: Stroke, after: Stroke },
    Transform { before: Vec<Stroke>, after: Vec<Stroke> },
}

pub struct TransformSession {
    pub ids: Vec<String>,
    pub before: Vec<Stroke>,
}

impl Session {
    pub fn new(strokes: Vec<Stroke>) -> Self {
        Self {
            strokes: Arc::new(RwLock::new(strokes)),
            active_ids: Arc::new(RwLock::new(HashSet::new())),
            owners: Arc::new(RwLock::new(HashMap::new())),
            histories: Arc::new(RwLock::new(HashMap::new())),
            peers: Arc::new(RwLock::new(HashMap::new())),
            transform_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}
