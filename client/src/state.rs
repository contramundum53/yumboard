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

#[derive(Clone, Copy, PartialEq)]
pub enum SelectionMode {
    None,
    Lasso,
    Move,
    Scale,
    Rotate,
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

pub struct State {
    pub canvas: HtmlCanvasElement,
    pub ctx: CanvasRenderingContext2d,
    pub strokes: Vec<Stroke>,
    pub active_ids: HashSet<String>,
    pub load_reader: Option<FileReader>,
    pub load_onload: Option<Closure<dyn FnMut(ProgressEvent)>>,
    pub board_width: f64,
    pub board_height: f64,
    pub board_scale: f64,
    pub board_offset_x: f64,
    pub board_offset_y: f64,
    pub zoom: f64,
    pub current_id: Option<String>,
    pub drawing: bool,
    pub erasing: bool,
    pub tool: Tool,
    pub erase_hits: HashSet<String>,
    pub panning: bool,
    pub pan_start_x: f64,
    pub pan_start_y: f64,
    pub pan_origin_x: f64,
    pub pan_origin_y: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    pub selected_ids: Vec<String>,
    pub lasso_points: Vec<Point>,
    pub palette: Vec<String>,
    pub palette_selected: Option<usize>,
    pub palette_last_selected: usize,
    pub palette_add_mode: bool,
    pub selection_mode: SelectionMode,
    pub transform_center: Point,
    pub transform_anchor: Point,
    pub transform_start: Point,
    pub transform_start_angle: f64,
    pub transform_snapshot: Vec<Stroke>,
    pub transform_scale_axis: ScaleAxis,
}
