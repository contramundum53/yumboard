use pfboard_shared::{Point, Stroke};

use crate::state::{ScaleAxis, ScaleHandle, SelectState, SelectionHit, State, STROKE_UNIT};

pub struct Bounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

pub fn normalize_point(point: Point) -> Option<Point> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return None;
    }
    Some(point)
}

pub fn world_to_screen(state: &State, point: Point) -> (f64, f64) {
    let x = point.x as f64 * state.zoom + state.board_offset_x + state.pan_x;
    let y = point.y as f64 * state.zoom + state.board_offset_y + state.pan_y;
    (x, y)
}

pub fn selection_bounds(strokes: &[Stroke], select: &SelectState) -> Option<Bounds> {
    if select.selected_ids.is_empty() {
        return None;
    }
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for stroke in strokes {
        if !select.selected_ids.iter().any(|id| id == &stroke.id) {
            continue;
        }
        for point in &stroke.points {
            min_x = min_x.min(point.x as f64);
            min_y = min_y.min(point.y as f64);
            max_x = max_x.max(point.x as f64);
            max_y = max_y.max(point.y as f64);
        }
    }
    if min_x == f64::MAX {
        None
    } else {
        Some(Bounds {
            min_x,
            min_y,
            max_x,
            max_y,
        })
    }
}

pub fn selection_center(strokes: &[Stroke], select: &SelectState) -> Option<Point> {
    let bounds = selection_bounds(strokes, select)?;
    Some(Point {
        x: ((bounds.min_x + bounds.max_x) / 2.0) as f32,
        y: ((bounds.min_y + bounds.max_y) / 2.0) as f32,
    })
}

pub fn selection_hit_test(
    state: &State,
    select: &SelectState,
    screen_x: f64,
    screen_y: f64,
) -> Option<SelectionHit> {
    let bounds = selection_bounds(&state.strokes, select)?;
    let (left, top) = world_to_screen(
        state,
        Point {
            x: bounds.min_x as f32,
            y: bounds.min_y as f32,
        },
    );
    let (right, bottom) = world_to_screen(
        state,
        Point {
            x: bounds.max_x as f32,
            y: bounds.max_y as f32,
        },
    );
    let handle = 10.0;
    let center_x = (left + right) / 2.0;
    let rotate_y = top - 24.0;
    if hit_rect(screen_x, screen_y, right + 18.0, top - 18.0, handle) {
        return Some(SelectionHit::Trash);
    }
    if hit_circle(screen_x, screen_y, center_x, rotate_y, 7.0) {
        return Some(SelectionHit::Rotate);
    }
    if hit_rect(screen_x, screen_y, left, top, handle) {
        return Some(SelectionHit::Scale(ScaleHandle {
            axis: ScaleAxis::Both,
            anchor: Point {
                x: bounds.max_x as f32,
                y: bounds.max_y as f32,
            },
        }));
    }
    if hit_rect(screen_x, screen_y, right, top, handle) {
        return Some(SelectionHit::Scale(ScaleHandle {
            axis: ScaleAxis::Both,
            anchor: Point {
                x: bounds.min_x as f32,
                y: bounds.max_y as f32,
            },
        }));
    }
    if hit_rect(screen_x, screen_y, left, bottom, handle) {
        return Some(SelectionHit::Scale(ScaleHandle {
            axis: ScaleAxis::Both,
            anchor: Point {
                x: bounds.max_x as f32,
                y: bounds.min_y as f32,
            },
        }));
    }
    if hit_rect(screen_x, screen_y, right, bottom, handle) {
        return Some(SelectionHit::Scale(ScaleHandle {
            axis: ScaleAxis::Both,
            anchor: Point {
                x: bounds.min_x as f32,
                y: bounds.min_y as f32,
            },
        }));
    }
    let mid_top_x = (left + right) / 2.0;
    let mid_left_y = (top + bottom) / 2.0;
    if hit_rect(screen_x, screen_y, mid_top_x, top, handle) {
        return Some(SelectionHit::Scale(ScaleHandle {
            axis: ScaleAxis::Y,
            anchor: Point {
                x: ((bounds.min_x + bounds.max_x) / 2.0) as f32,
                y: bounds.max_y as f32,
            },
        }));
    }
    if hit_rect(screen_x, screen_y, mid_top_x, bottom, handle) {
        return Some(SelectionHit::Scale(ScaleHandle {
            axis: ScaleAxis::Y,
            anchor: Point {
                x: ((bounds.min_x + bounds.max_x) / 2.0) as f32,
                y: bounds.min_y as f32,
            },
        }));
    }
    if hit_rect(screen_x, screen_y, left, mid_left_y, handle) {
        return Some(SelectionHit::Scale(ScaleHandle {
            axis: ScaleAxis::X,
            anchor: Point {
                x: bounds.max_x as f32,
                y: ((bounds.min_y + bounds.max_y) / 2.0) as f32,
            },
        }));
    }
    if hit_rect(screen_x, screen_y, right, mid_left_y, handle) {
        return Some(SelectionHit::Scale(ScaleHandle {
            axis: ScaleAxis::X,
            anchor: Point {
                x: bounds.min_x as f32,
                y: ((bounds.min_y + bounds.max_y) / 2.0) as f32,
            },
        }));
    }
    if screen_x >= left && screen_x <= right && screen_y >= top && screen_y <= bottom {
        return Some(SelectionHit::Move);
    }
    None
}

fn hit_rect(x: f64, y: f64, cx: f64, cy: f64, size: f64) -> bool {
    let half = size / 2.0;
    x >= cx - half && x <= cx + half && y >= cy - half && y <= cy + half
}

fn hit_circle(x: f64, y: f64, cx: f64, cy: f64, radius: f64) -> bool {
    let dx = x - cx;
    let dy = y - cy;
    dx * dx + dy * dy <= radius * radius
}

pub fn angle_between(center: Point, point: Point) -> f64 {
    let dx = point.x as f64 - center.x as f64;
    let dy = point.y as f64 - center.y as f64;
    dy.atan2(dx)
}

pub fn selected_strokes(strokes: &[Stroke], select: &SelectState) -> Vec<Stroke> {
    strokes
        .iter()
        .filter(|stroke| select.selected_ids.iter().any(|id| id == &stroke.id))
        .cloned()
        .collect()
}

pub fn apply_translation(strokes: &[Stroke], dx: f32, dy: f32) -> Vec<Stroke> {
    strokes
        .iter()
        .map(|stroke| Stroke {
            id: stroke.id.clone(),
            color: stroke.color.clone(),
            size: stroke.size,
            points: stroke
                .points
                .iter()
                .map(|point| Point {
                    x: point.x + dx,
                    y: point.y + dy,
                })
                .collect(),
        })
        .collect()
}

pub fn apply_scale_xy(strokes: &[Stroke], center: Point, sx: f64, sy: f64) -> Vec<Stroke> {
    let cx = center.x as f64;
    let cy = center.y as f64;
    strokes
        .iter()
        .map(|stroke| Stroke {
            id: stroke.id.clone(),
            color: stroke.color.clone(),
            size: stroke.size,
            points: stroke
                .points
                .iter()
                .map(|point| Point {
                    x: (cx + (point.x as f64 - cx) * sx) as f32,
                    y: (cy + (point.y as f64 - cy) * sy) as f32,
                })
                .collect(),
        })
        .collect()
}

pub fn clamp_scale(value: f64, min_abs: f64) -> f64 {
    if value.abs() < min_abs {
        if value.is_sign_negative() {
            -min_abs
        } else {
            min_abs
        }
    } else {
        value
    }
}

pub fn apply_rotation(strokes: &[Stroke], center: Point, angle: f64) -> Vec<Stroke> {
    let cx = center.x as f64;
    let cy = center.y as f64;
    let cos = angle.cos();
    let sin = angle.sin();
    strokes
        .iter()
        .map(|stroke| Stroke {
            id: stroke.id.clone(),
            color: stroke.color.clone(),
            size: stroke.size,
            points: stroke
                .points
                .iter()
                .map(|point| {
                    let dx = point.x as f64 - cx;
                    let dy = point.y as f64 - cy;
                    Point {
                        x: (cx + dx * cos - dy * sin) as f32,
                        y: (cy + dx * sin + dy * cos) as f32,
                    }
                })
                .collect(),
        })
        .collect()
}

pub fn point_in_polygon(point: Point, polygon: &[Point]) -> bool {
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let pi = polygon[i];
        let pj = polygon[j];
        let xi = pi.x as f64;
        let yi = pi.y as f64;
        let xj = pj.x as f64;
        let yj = pj.y as f64;
        let px = point.x as f64;
        let py = point.y as f64;
        let intersect = ((yi > py) != (yj > py))
            && (px < (xj - xi) * (py - yi) / (yj - yi + f64::EPSILON) + xi);
        if intersect {
            inside = !inside;
        }
        j = i;
    }
    inside
}

pub fn distance_to_segment(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let dx = x2 - x1;
    let dy = y2 - y1;
    if dx.abs() < f64::EPSILON && dy.abs() < f64::EPSILON {
        return ((px - x1).powi(2) + (py - y1).powi(2)).sqrt();
    }
    let t = ((px - x1) * dx + (py - y1) * dy) / (dx * dx + dy * dy);
    let t = t.clamp(0.0, 1.0);
    let proj_x = x1 + t * dx;
    let proj_y = y1 + t * dy;
    ((px - proj_x).powi(2) + (py - proj_y).powi(2)).sqrt()
}

pub fn stroke_hit(
    stroke: &Stroke,
    px: f64,
    py: f64,
    zoom: f64,
    offset_x: f64,
    offset_y: f64,
    pan_x: f64,
    pan_y: f64,
) -> bool {
    if stroke.points.is_empty() {
        return false;
    }
    let threshold = (stroke.size as f64 * zoom * STROKE_UNIT / 2.0).max(6.0);
    if stroke.points.len() == 1 {
        let point = stroke.points[0];
        let dx = point.x as f64 * zoom + offset_x + pan_x - px;
        let dy = point.y as f64 * zoom + offset_y + pan_y - py;
        return dx * dx + dy * dy <= threshold * threshold;
    }
    for window in stroke.points.windows(2) {
        let start = window[0];
        let end = window[1];
        let distance = distance_to_segment(
            px,
            py,
            start.x as f64 * zoom + offset_x + pan_x,
            start.y as f64 * zoom + offset_y + pan_y,
            end.x as f64 * zoom + offset_x + pan_x,
            end.y as f64 * zoom + offset_y + pan_y,
        );
        if distance <= threshold {
            return true;
        }
    }
    false
}
