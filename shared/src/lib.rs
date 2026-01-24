use serde::{Deserialize, Serialize};

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
    pub id: String,
    pub color: String,
    pub size: f32,
    pub points: Vec<Point>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "stroke:start")]
    StrokeStart {
        id: String,
        color: String,
        size: f32,
        point: Point,
    },
    #[serde(rename = "stroke:move")]
    StrokeMove { id: String, point: Point },
    #[serde(rename = "stroke:end")]
    StrokeEnd { id: String },
    #[serde(rename = "clear")]
    Clear,
    #[serde(rename = "undo")]
    Undo,
    #[serde(rename = "redo")]
    Redo,
    #[serde(rename = "erase")]
    Erase { id: String },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "sync")]
    Sync { strokes: Vec<Stroke> },
    #[serde(rename = "stroke:start")]
    StrokeStart {
        id: String,
        color: String,
        size: f32,
        point: Point,
    },
    #[serde(rename = "stroke:move")]
    StrokeMove { id: String, point: Point },
    #[serde(rename = "stroke:end")]
    StrokeEnd { id: String },
    #[serde(rename = "clear")]
    Clear,
    #[serde(rename = "stroke:remove")]
    StrokeRemove { id: String },
    #[serde(rename = "stroke:restore")]
    StrokeRestore { stroke: Stroke },
}
