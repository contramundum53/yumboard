use web_sys::CanvasRenderingContext2d;

use pfboard_shared::{Point, Stroke};

use crate::geometry::{selection_bounds, world_to_screen};
use crate::state::State;

pub fn draw_dot(
    ctx: &CanvasRenderingContext2d,
    board_scale: f64,
    board_offset_x: f64,
    board_offset_y: f64,
    zoom: f64,
    pan_x: f64,
    pan_y: f64,
    point: Point,
    color: &str,
    size: f32,
) {
    let scale = board_scale * zoom;
    let weight = size as f64 * zoom;
    let x = point.x as f64 * scale + board_offset_x + pan_x;
    let y = point.y as f64 * scale + board_offset_y + pan_y;
    ctx.set_fill_style_str(color);
    ctx.begin_path();
    let _ = ctx.arc(x, y, weight / 2.0, 0.0, std::f64::consts::PI * 2.0);
    ctx.fill();
}

pub fn draw_segment(
    ctx: &CanvasRenderingContext2d,
    board_scale: f64,
    board_offset_x: f64,
    board_offset_y: f64,
    zoom: f64,
    pan_x: f64,
    pan_y: f64,
    from: Point,
    to: Point,
    color: &str,
    size: f32,
) {
    let scale = board_scale * zoom;
    let weight = size as f64 * zoom;
    let from_x = from.x as f64 * scale + board_offset_x + pan_x;
    let from_y = from.y as f64 * scale + board_offset_y + pan_y;
    let to_x = to.x as f64 * scale + board_offset_x + pan_x;
    let to_y = to.y as f64 * scale + board_offset_y + pan_y;

    ctx.set_stroke_style_str(color);
    ctx.set_line_width(weight);
    ctx.begin_path();
    ctx.move_to(from_x, from_y);
    ctx.line_to(to_x, to_y);
    ctx.stroke();
}

pub fn draw_stroke(state: &State, stroke: &Stroke) {
    if stroke.points.is_empty() {
        return;
    }
    if stroke.points.len() == 1 {
        draw_dot(
            &state.ctx,
            state.board_scale,
            state.board_offset_x,
            state.board_offset_y,
            state.zoom,
            state.pan_x,
            state.pan_y,
            stroke.points[0],
            &stroke.color,
            stroke.size,
        );
        return;
    }
    for i in 1..stroke.points.len() {
        draw_segment(
            &state.ctx,
            state.board_scale,
            state.board_offset_x,
            state.board_offset_y,
            state.zoom,
            state.pan_x,
            state.pan_y,
            stroke.points[i - 1],
            stroke.points[i],
            &stroke.color,
            stroke.size,
        );
    }
}

pub fn redraw(state: &mut State) {
    state
        .ctx
        .clear_rect(0.0, 0.0, state.board_width, state.board_height);
    for stroke in &state.strokes {
        draw_stroke(state, stroke);
    }
    draw_selection_overlay(state);
}

pub fn draw_selection_overlay(state: &mut State) {
    if state.selected_ids.is_empty() && state.lasso_points.is_empty() {
        return;
    }
    let ctx = &state.ctx;
    ctx.save();
    ctx.set_line_width(1.5);
    ctx.set_stroke_style_str("rgba(26, 31, 42, 0.65)");
    ctx.set_fill_style_str("rgba(26, 31, 42, 0.08)");

    if !state.lasso_points.is_empty() {
        let mut first = true;
        ctx.begin_path();
        let _ = ctx.set_line_dash(&js_sys::Array::of2(&4.into(), &6.into()));
        for point in &state.lasso_points {
            let (x, y) = world_to_screen(state, *point);
            if first {
                ctx.move_to(x, y);
                first = false;
            } else {
                ctx.line_to(x, y);
            }
        }
        ctx.stroke();
        let _ = ctx.set_line_dash(&js_sys::Array::new());
    }

    if let Some(bounds) = selection_bounds(state) {
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
        let width = right - left;
        let height = bottom - top;
        ctx.stroke_rect(left, top, width, height);

        let handle = 10.0;
        let center_x = (left + right) / 2.0;
        let rotate_y = top - 24.0;
        draw_handle(ctx, left, top, handle);
        draw_handle(ctx, right, top, handle);
        draw_handle(ctx, left, bottom, handle);
        draw_handle(ctx, right, bottom, handle);
        draw_handle(ctx, center_x, top, handle);
        draw_handle(ctx, center_x, bottom, handle);
        draw_handle(ctx, left, (top + bottom) / 2.0, handle);
        draw_handle(ctx, right, (top + bottom) / 2.0, handle);
        draw_handle_circle(ctx, center_x, rotate_y, 6.0);
        draw_trash_handle(ctx, right + 18.0, top - 18.0, handle);
    }

    ctx.restore();
}

fn draw_handle(ctx: &CanvasRenderingContext2d, x: f64, y: f64, size: f64) {
    let half = size / 2.0;
    ctx.set_fill_style_str("rgba(26, 31, 42, 0.85)");
    ctx.fill_rect(x - half, y - half, size, size);
}

fn draw_handle_circle(ctx: &CanvasRenderingContext2d, x: f64, y: f64, radius: f64) {
    ctx.set_fill_style_str("rgba(26, 31, 42, 0.85)");
    ctx.begin_path();
    let _ = ctx.arc(x, y, radius, 0.0, std::f64::consts::PI * 2.0);
    ctx.fill();
}

fn draw_trash_handle(ctx: &CanvasRenderingContext2d, x: f64, y: f64, size: f64) {
    let half = size / 2.0;
    ctx.set_fill_style_str("rgba(228, 107, 73, 0.95)");
    ctx.fill_rect(x - half, y - half, size, size);
    ctx.set_stroke_style_str("#fff");
    ctx.set_line_width(2.0);
    ctx.begin_path();
    ctx.move_to(x - half + 2.0, y - half + 2.0);
    ctx.line_to(x + half - 2.0, y + half - 2.0);
    ctx.move_to(x + half - 2.0, y - half + 2.0);
    ctx.line_to(x - half + 2.0, y + half - 2.0);
    ctx.stroke();
}
