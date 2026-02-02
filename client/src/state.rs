use std::collections::{HashMap, HashSet};

use wasm_bindgen::prelude::Closure;
use web_sys::{FileReader, ProgressEvent};

use yumboard_shared::{Point, Stroke, StrokeId};

pub const DEFAULT_PALETTE: [&str; 3] = ["#1f1f1f", "#d60000", "#0000d0"];
pub const STROKE_UNIT: f64 = 1.0;

#[derive(Clone, Copy)]
pub enum ScaleAxis {
    Both,
    X,
    Y,
}

#[derive(Clone, Copy)]
pub struct ScaleHandle {
    pub axis: ScaleAxis,
    pub anchor: Point,
}

pub enum SelectionHit {
    Move,
    Scale(ScaleHandle),
    Rotate,
    Trash,
}

pub enum DrawMode {
    Idle,
    Drawing { id: StrokeId },
}

pub struct DrawState {
    pub mode: DrawMode,
    pub palette_selected: usize,
}

pub enum EraseMode {
    Idle,
    Active { hits: HashSet<StrokeId> },
}

pub enum PanMode {
    Idle,
    Active {
        start_x: f64,
        start_y: f64,
        origin_x: f64,
        origin_y: f64,
    },
}

pub enum SelectMode {
    Idle,
    Lasso {
        points: Vec<Point>,
    },
    Move {
        start: Point,
        snapshot: Vec<Stroke>,
        last_dx: f32,
        last_dy: f32,
    },
    Scale {
        anchor: Point,
        start: Point,
        axis: ScaleAxis,
        snapshot: Vec<Stroke>,
        last_sx: f64,
        last_sy: f64,
    },
    Rotate {
        center: Point,
        start_angle: f64,
        snapshot: Vec<Stroke>,
        last_delta: f64,
    },
}

pub struct SelectState {
    pub selected_ids: Vec<StrokeId>,
    pub mode: SelectMode,
}

pub struct LoadingState {
    pub previous: Box<Mode>,
    pub reader: Option<FileReader>,
    pub onload: Option<Closure<dyn FnMut(ProgressEvent)>>,
}

pub struct PinchState {
    pub world_center_x: f64,
    pub world_center_y: f64,
    pub distance: f64,
    pub zoom: f64,
}

pub struct DrawPointerState {
    pub pointer_id: i32,
    pub last_timestamp: f64,
}

pub enum InputActivity {
    None,
    Draw(DrawPointerState),
    Pinch(PinchState),
    Pan(PanMode),
}

pub enum Mode {
    Draw(DrawState),
    Erase(EraseMode),
    Pan(PanMode),
    Select(SelectState),
    Loading(LoadingState),
}

pub struct State {
    pub strokes: Vec<Stroke>,
    pub active_ids: HashSet<StrokeId>,
    pub board_width: f64,
    pub board_height: f64,
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    pub palette: Vec<String>,
    pub mode: Mode,
    pub pending_points: HashMap<StrokeId, Vec<Point>>,
    pub flush_scheduled: bool,
    pub input_activity: InputActivity,
    pub touch_points: HashMap<i32, (f64, f64)>,
}

impl State {}
