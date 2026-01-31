use std::collections::HashSet;

use yumboard_shared::{Point, Stroke, StrokeId, TransformOp};

use crate::geometry::{home_zoom_pan, normalize_point, stroke_hit};
use crate::render::{draw_dot, draw_segment, redraw};
use crate::state::{EraseMode, Mode, SelectMode, State};

pub fn sanitize_color(mut color: String) -> String {
    if color.is_empty() {
        return "#1f1f1f".to_string();
    }
    if color.len() > 32 {
        color.truncate(32);
    }
    color
}

pub fn sanitize_size(size: f32) -> f32 {
    let size = if size.is_finite() { size } else { 6.0 };
    size.max(1.0).min(60.0)
}

pub fn start_stroke(state: &mut State, id: StrokeId, color: String, size: f32, point: Point) {
    let point = match normalize_point(point) {
        Some(point) => point,
        None => return,
    };
    let color = sanitize_color(color);
    let size = sanitize_size(size);
    let stroke = Stroke {
        id: id.clone(),
        color: color.clone(),
        size,
        points: vec![point],
    };
    state.strokes.push(stroke);
    state.active_ids.insert(id);
    draw_dot(
        &state.ctx,
        state.zoom,
        state.pan_x,
        state.pan_y,
        point,
        &color,
        size,
    );
}

pub fn move_stroke(state: &mut State, id: &StrokeId, point: Point) -> bool {
    let point = match normalize_point(point) {
        Some(point) => point,
        None => return false,
    };
    if !state.active_ids.contains(id) {
        return false;
    }
    let mut draw_action = None;
    if let Some(stroke) = state
        .strokes
        .iter_mut()
        .rev()
        .find(|stroke| &stroke.id == id)
    {
        if let Some(last) = stroke.points.last().copied() {
            if last == point {
                return false;
            }
            stroke.points.push(point);
            draw_action = Some((last, point, stroke.color.clone(), stroke.size));
        } else {
            stroke.points.push(point);
            draw_action = Some((point, point, stroke.color.clone(), stroke.size));
        }
    }
    if let Some((from, to, color, size)) = draw_action {
        if from == to {
            draw_dot(
                &state.ctx,
                state.zoom,
                state.pan_x,
                state.pan_y,
                to,
                &color,
                size,
            );
        } else {
            draw_segment(
                &state.ctx,
                state.zoom,
                state.pan_x,
                state.pan_y,
                from,
                to,
                &color,
                size,
            );
        }
        return true;
    }
    false
}

pub fn end_stroke(state: &mut State, id: &StrokeId) {
    state.active_ids.remove(id);
}

pub fn clear_board(state: &mut State) {
    state.strokes.clear();
    state.active_ids.clear();
    if let Mode::Select(select) = &mut state.mode {
        select.selected_ids.clear();
        select.mode = SelectMode::Idle;
    }
    redraw(state);
}

pub fn remove_stroke(state: &mut State, id: &StrokeId) {
    if let Some(index) = state.strokes.iter().position(|stroke| &stroke.id == id) {
        state.strokes.remove(index);
        state.active_ids.remove(id);
    }
}

pub fn replace_stroke_local(state: &mut State, stroke: Stroke) {
    if let Some(index) = state.strokes.iter().position(|item| item.id == stroke.id) {
        state.strokes[index] = stroke;
    }
}

pub fn restore_stroke(state: &mut State, mut stroke: Stroke) {
    stroke.points = stroke
        .points
        .into_iter()
        .filter_map(normalize_point)
        .collect();
    state.strokes.push(stroke);
    redraw(state);
}

pub fn erase_hits_at_point(state: &mut State, point: Point) -> Vec<StrokeId> {
    let hits = match &mut state.mode {
        Mode::Erase(EraseMode::Active { hits }) => hits,
        _ => return Vec::new(),
    };
    let px = point.x as f64 * state.zoom + state.pan_x;
    let py = point.y as f64 * state.zoom + state.pan_y;
    let mut removed = Vec::new();
    let mut index = state.strokes.len();

    while index > 0 {
        index -= 1;
        let stroke = &state.strokes[index];
        if hits.contains(&stroke.id) {
            continue;
        }
        if stroke_hit(stroke, px, py, state.zoom, state.pan_x, state.pan_y) {
            let id = stroke.id.clone();
            state.strokes.remove(index);
            state.active_ids.remove(&id);
            hits.insert(id.clone());
            removed.push(id);
        }
    }

    if !removed.is_empty() {
        redraw(state);
    }

    removed
}

pub fn adopt_strokes(state: &mut State, strokes: Vec<Stroke>) {
    let mut sanitized = Vec::with_capacity(strokes.len());
    for mut stroke in strokes {
        stroke.points = stroke
            .points
            .into_iter()
            .filter_map(normalize_point)
            .collect();
        sanitized.push(stroke);
    }
    state.strokes = sanitized;
    state.active_ids.clear();
    if let Mode::Select(select) = &mut state.mode {
        select.selected_ids.clear();
        select.mode = SelectMode::Idle;
    }

    let (zoom, pan_x, pan_y) = home_zoom_pan(&state);
    state.zoom = zoom;
    state.pan_x = pan_x;
    state.pan_y = pan_y;
    redraw(state);
}

pub fn apply_transformed_strokes(state: &mut State, strokes: &[Stroke]) {
    for stroke in strokes {
        replace_stroke_local(state, stroke.clone());
    }
    redraw(state);
}

pub fn apply_transform_operation(state: &mut State, ids: &[StrokeId], op: &TransformOp) {
    if ids.is_empty() {
        return;
    }
    let id_set: HashSet<&StrokeId> = ids.iter().collect();
    match *op {
        TransformOp::Translate { dx, dy } => {
            if !dx.is_finite() || !dy.is_finite() {
                return;
            }
            for stroke in &mut state.strokes {
                if !id_set.contains(&stroke.id) {
                    continue;
                }
                for point in &mut stroke.points {
                    point.x = (point.x as f64 + dx) as f32;
                    point.y = (point.y as f64 + dy) as f32;
                }
            }
        }
        TransformOp::Scale { anchor, sx, sy } => {
            if !sx.is_finite() || !sy.is_finite() {
                return;
            }
            let cx = anchor.x as f64;
            let cy = anchor.y as f64;
            for stroke in &mut state.strokes {
                if !id_set.contains(&stroke.id) {
                    continue;
                }
                for point in &mut stroke.points {
                    let dx = point.x as f64 - cx;
                    let dy = point.y as f64 - cy;
                    point.x = (cx + dx * sx) as f32;
                    point.y = (cy + dy * sy) as f32;
                }
            }
        }
        TransformOp::Rotate { center, delta } => {
            if !delta.is_finite() {
                return;
            }
            let cx = center.x as f64;
            let cy = center.y as f64;
            let cos = delta.cos();
            let sin = delta.sin();
            for stroke in &mut state.strokes {
                if !id_set.contains(&stroke.id) {
                    continue;
                }
                for point in &mut stroke.points {
                    let dx = point.x as f64 - cx;
                    let dy = point.y as f64 - cy;
                    point.x = (cx + dx * cos - dy * sin) as f32;
                    point.y = (cy + dx * sin + dy * cos) as f32;
                }
            }
        }
    }
}

pub fn finalize_lasso_selection(state: &mut State) {
    let select = match &mut state.mode {
        Mode::Select(select) => select,
        _ => return,
    };
    let points = match &mut select.mode {
        SelectMode::Lasso { points } => points,
        _ => return,
    };
    if points.len() < 3 {
        points.clear();
        return;
    }
    let polygon = points.clone();
    let mut selected = Vec::new();
    for stroke in &state.strokes {
        let mut inside = false;
        for point in &stroke.points {
            if crate::geometry::point_in_polygon(*point, &polygon) {
                inside = true;
                break;
            }
        }
        if inside {
            selected.push(stroke.id.clone());
        }
    }
    select.selected_ids = selected;
}
