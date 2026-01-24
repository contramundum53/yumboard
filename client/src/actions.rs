use pfboard_shared::{Point, Stroke};

use crate::geometry::{normalize_point, stroke_hit};
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

pub fn start_stroke(state: &mut State, id: String, color: String, size: f32, point: Point) {
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
        state.board_offset_x,
        state.board_offset_y,
        state.zoom,
        state.pan_x,
        state.pan_y,
        point,
        &color,
        size,
    );
}

pub fn move_stroke(state: &mut State, id: &str, point: Point) {
    let point = match normalize_point(point) {
        Some(point) => point,
        None => return,
    };
    if !state.active_ids.contains(id) {
        return;
    }
    let mut draw_action = None;
    if let Some(stroke) = state.strokes.iter_mut().find(|stroke| stroke.id == id) {
        if let Some(last) = stroke.points.last().copied() {
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
                state.board_offset_x,
                state.board_offset_y,
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
                state.board_offset_x,
                state.board_offset_y,
                state.zoom,
                state.pan_x,
                state.pan_y,
                from,
                to,
                &color,
                size,
            );
        }
    }
}

pub fn end_stroke(state: &mut State, id: &str) {
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

pub fn remove_stroke(state: &mut State, id: &str) {
    if let Some(index) = state.strokes.iter().position(|stroke| stroke.id == id) {
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

pub fn erase_hits_at_point(state: &mut State, point: Point) -> Vec<String> {
    let hits = match &mut state.mode {
        Mode::Erase(EraseMode::Active { hits }) => hits,
        _ => return Vec::new(),
    };
    let px = point.x as f64 * state.zoom + state.board_offset_x + state.pan_x;
    let py = point.y as f64 * state.zoom + state.board_offset_y + state.pan_y;
    let mut removed = Vec::new();
    let mut index = state.strokes.len();

    while index > 0 {
        index -= 1;
        let stroke = &state.strokes[index];
        if hits.contains(&stroke.id) {
            continue;
        }
        if stroke_hit(
            stroke,
            px,
            py,
            state.zoom,
            state.board_offset_x,
            state.board_offset_y,
            state.pan_x,
            state.pan_y,
        ) {
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
    redraw(state);
}

pub fn apply_transformed_strokes(state: &mut State, strokes: &[Stroke]) {
    for stroke in strokes {
        replace_stroke_local(state, stroke.clone());
    }
    redraw(state);
}

pub fn last_point_for_id(strokes: &[Stroke], id: &str) -> Option<Point> {
    strokes
        .iter()
        .find(|stroke| stroke.id == id)
        .and_then(|stroke| stroke.points.last().copied())
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
