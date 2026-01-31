use std::collections::{HashMap, HashSet};

use wasm_bindgen::prelude::Closure;
use web_sys::{CanvasRenderingContext2d, FileReader, HtmlCanvasElement, ProgressEvent};

use yumboard_shared::{Point, Stroke};

pub const DEFAULT_PALETTE: [&str; 1] = ["#1f1f1f"];
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
    Drawing { id: String },
}

pub struct DrawState {
    pub mode: DrawMode,
    pub palette_selected: usize,
}

pub enum EraseMode {
    Idle,
    Active { hits: HashSet<String> },
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
    },
    Scale {
        anchor: Point,
        start: Point,
        axis: ScaleAxis,
        snapshot: Vec<Stroke>,
    },
    Rotate {
        center: Point,
        start_angle: f64,
        snapshot: Vec<Stroke>,
    },
}

pub struct SelectState {
    pub selected_ids: Vec<String>,
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

pub enum Mode {
    Draw(DrawState),
    Erase(EraseMode),
    Pan(PanMode),
    Select(SelectState),
    Loading(LoadingState),
}

pub struct State {
    pub canvas: HtmlCanvasElement,
    pub ctx: CanvasRenderingContext2d,
    pub strokes: Vec<Stroke>,
    pub active_ids: HashSet<String>,
    pub board_width: f64,
    pub board_height: f64,
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    pub palette: Vec<String>,
    pub mode: Mode,
    pub touch_points: HashMap<i32, (f64, f64)>,
    pub pinch: Option<PinchState>,
}

impl State {}
