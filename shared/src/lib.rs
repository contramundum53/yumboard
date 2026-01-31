use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Encode, Decode, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct StrokeId([u64; 2]);

impl StrokeId {
    pub fn new(value: [u64; 2]) -> Self {
        Self(value)
    }
}

#[derive(Serialize, Deserialize, Encode, Decode, Clone, Copy, Debug, PartialEq)]
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

#[derive(Serialize, Deserialize, Encode, Decode, Clone, Debug)]
pub struct Stroke {
    pub id: StrokeId,
    pub color: Color,
    pub size: f32,
    pub points: Vec<Point>,
}

#[derive(Serialize, Deserialize, Encode, Decode, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const DEFAULT: Color = Color {
        r: 0x1f,
        g: 0x1f,
        b: 0x1f,
        a: 0xff,
    };

    pub fn from_hex(input: &str) -> Option<Color> {
        let trimmed = input.trim();
        let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
        match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
                Some(Color {
                    r: (r << 4) | r,
                    g: (g << 4) | g,
                    b: (b << 4) | b,
                    a: 0xff,
                })
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Color { r, g, b, a: 0xff })
            }
            _ => None,
        }
    }

    pub fn to_rgba_css(self) -> String {
        let alpha = self.a as f32 / 255.0;
        format!("rgba({}, {}, {}, {})", self.r, self.g, self.b, alpha)
    }
}

#[derive(Serialize, Deserialize, Encode, Decode, Clone, Debug)]
#[serde(tag = "op")]
pub enum TransformOp {
    #[serde(rename = "translate")]
    Translate { dx: f64, dy: f64 },
    #[serde(rename = "scale")]
    Scale { anchor: Point, sx: f64, sy: f64 },
    #[serde(rename = "rotate")]
    Rotate { center: Point, delta: f64 },
}

#[derive(Serialize, Deserialize, Encode, Decode, Clone, Debug)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "stroke:start")]
    StrokeStart {
        id: StrokeId,
        color: Color,
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

#[derive(Serialize, Deserialize, Encode, Decode, Clone, Debug)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "sync")]
    Sync { strokes: Vec<Stroke> },
    #[serde(rename = "stroke:start")]
    StrokeStart {
        id: StrokeId,
        color: Color,
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
