use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;
use yumboard_shared::Stroke;

pub const MAX_STROKES: usize = 2000;
pub const MAX_POINTS_PER_STROKE: usize = 5000;

#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<RwLock<HashMap<String, Arc<RwLock<Session>>>>>,
    pub session_dir: PathBuf,
}

pub struct Session {
    pub strokes: Vec<Stroke>,
    pub active_ids: HashSet<String>,
    pub owners: HashMap<String, Uuid>,
    pub histories: HashMap<Uuid, ClientHistory>,
    pub peers: HashMap<Uuid, mpsc::UnboundedSender<yumboard_shared::ServerMessage>>,
    pub transform_sessions: HashMap<Uuid, TransformSession>,
    pub dirty: bool,
}

#[derive(Default)]
pub struct ClientHistory {
    pub undo: Vec<Action>,
    pub redo: Vec<Action>,
}

pub enum Action {
    AddStroke(Stroke),
    EraseStroke(Stroke),
    Clear {
        strokes: Vec<Stroke>,
    },
    ReplaceStroke {
        before: Stroke,
        after: Stroke,
    },
    Transform {
        before: Vec<Stroke>,
        after: Vec<Stroke>,
    },
}

pub struct TransformSession {
    pub ids: Vec<String>,
    pub before: Vec<Stroke>,
}

impl Session {
    pub fn new(strokes: Vec<Stroke>) -> Self {
        Self {
            strokes,
            active_ids: HashSet::new(),
            owners: HashMap::new(),
            histories: HashMap::new(),
            peers: HashMap::new(),
            transform_sessions: HashMap::new(),
            dirty: false,
        }
    }
}
