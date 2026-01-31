use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct StrokeId([u64; 2]);

impl StrokeId {
    pub fn new(value: [u64; 2]) -> Self {
        Self(value)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn clamp(self) -> Self {
        Self {
            x: clamp_unit(self.x),
            y: clamp_unit(self.y),
        }
    }
}

fn clamp_unit(value: f32) -> f32 {
    value.max(0.0).min(1.0)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Stroke {
    pub id: StrokeId,
    pub color: String,
    pub size: f32,
    pub points: Vec<Point>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "op")]
pub enum TransformOp {
    #[serde(rename = "translate")]
    Translate { dx: f64, dy: f64 },
    #[serde(rename = "scale")]
    Scale { anchor: Point, sx: f64, sy: f64 },
    #[serde(rename = "rotate")]
    Rotate { center: Point, delta: f64 },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "stroke:start")]
    StrokeStart {
        id: StrokeId,
        color: String,
        size: f32,
        point: Point,
    },
    #[serde(rename = "stroke:move")]
    StrokeMove { id: StrokeId, point: Point },
    #[serde(rename = "stroke:points")]
    StrokePoints { id: StrokeId, points: Vec<Point> },
    #[serde(rename = "stroke:end")]
    StrokeEnd { id: StrokeId },
    #[serde(rename = "clear")]
    Clear,
    #[serde(rename = "undo")]
    Undo,
    #[serde(rename = "redo")]
    Redo,
    #[serde(rename = "erase")]
    Erase { id: StrokeId },
    #[serde(rename = "stroke:replace")]
    StrokeReplace { stroke: Stroke },
    #[serde(rename = "transform:update")]
    TransformUpdate {
        ids: Vec<StrokeId>,
        #[serde(flatten)]
        op: TransformOp,
    },
    #[serde(rename = "transform:start")]
    TransformStart { ids: Vec<StrokeId> },
    #[serde(rename = "transform:end")]
    TransformEnd { ids: Vec<StrokeId> },
    #[serde(rename = "remove")]
    Remove { ids: Vec<StrokeId> },
    #[serde(rename = "load")]
    Load { strokes: Vec<Stroke> },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "sync")]
    Sync { strokes: Vec<Stroke> },
    #[serde(rename = "stroke:start")]
    StrokeStart {
        id: StrokeId,
        color: String,
        size: f32,
        point: Point,
    },
    #[serde(rename = "stroke:move")]
    StrokeMove { id: StrokeId, point: Point },
    #[serde(rename = "stroke:points")]
    StrokePoints { id: StrokeId, points: Vec<Point> },
    #[serde(rename = "stroke:end")]
    StrokeEnd { id: StrokeId },
    #[serde(rename = "clear")]
    Clear,
    #[serde(rename = "stroke:remove")]
    StrokeRemove { id: StrokeId },
    #[serde(rename = "stroke:restore")]
    StrokeRestore { stroke: Stroke },
    #[serde(rename = "stroke:replace")]
    StrokeReplace { stroke: Stroke },
    #[serde(rename = "transform:update")]
    TransformUpdate {
        ids: Vec<StrokeId>,
        #[serde(flatten)]
        op: TransformOp,
    },
}
