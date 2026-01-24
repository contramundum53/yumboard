use std::collections::HashSet;

use wasm_bindgen::prelude::Closure;
use web_sys::{CanvasRenderingContext2d, FileReader, HtmlCanvasElement, ProgressEvent};

use pfboard_shared::{Point, Stroke};

pub const DEFAULT_PALETTE: [&str; 1] = ["#1f1f1f"];
pub const STROKE_UNIT: f64 = 1.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Tool {
    Draw,
    Erase,
    Pan,
    Select,
}

pub enum SelectionHit {
    Move,
    Scale(ScaleHandle),
    Rotate,
    Trash,
}

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

pub enum DrawMode {
    Idle,
    Drawing { id: String },
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
    Lasso { points: Vec<Point> },
    Move { start: Point, snapshot: Vec<Stroke> },
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

pub enum Mode {
    Draw(DrawMode),
    Erase(EraseMode),
    Pan(PanMode),
    Select(SelectMode),
}

pub struct State {
    pub canvas: HtmlCanvasElement,
    pub ctx: CanvasRenderingContext2d,
    pub strokes: Vec<Stroke>,
    pub active_ids: HashSet<String>,
    pub load_reader: Option<FileReader>,
    pub load_onload: Option<Closure<dyn FnMut(ProgressEvent)>>,
    pub board_width: f64,
    pub board_height: f64,
    pub board_offset_x: f64,
    pub board_offset_y: f64,
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    pub selected_ids: Vec<String>,
    pub palette: Vec<String>,
    pub palette_selected: Option<usize>,
    pub palette_last_selected: usize,
    pub palette_add_mode: bool,
    pub mode: Mode,
}

impl Mode {
    pub fn tool(&self) -> Tool {
        match self {
            Mode::Draw(_) => Tool::Draw,
            Mode::Erase(_) => Tool::Erase,
            Mode::Pan(_) => Tool::Pan,
            Mode::Select(_) => Tool::Select,
        }
    }
}

impl State {
    pub fn tool(&self) -> Tool {
        self.mode.tool()
    }

    pub fn lasso_points(&self) -> &[Point] {
        match &self.mode {
            Mode::Select(SelectMode::Lasso { points }) => points,
            _ => &[],
        }
    }
}
