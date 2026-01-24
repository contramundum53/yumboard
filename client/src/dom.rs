use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    Document, Element, HtmlCanvasElement, HtmlElement, HtmlInputElement, HtmlSpanElement,
    PointerEvent, Window,
};

use pfboard_shared::Point;

use crate::geometry::normalize_point;
use crate::render::redraw;
use crate::state::{Mode, State};

pub fn get_element<T: JsCast>(document: &Document, id: &str) -> Result<T, JsValue> {
    let element = document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("Missing element: {id}")))?;
    element
        .dyn_into::<T>()
        .map_err(|_| JsValue::from_str(&format!("Invalid element type: {id}")))
}

pub fn update_size_label(input: &HtmlInputElement, value: &HtmlSpanElement) {
    value.set_text_content(Some(&input.value()));
}

pub fn set_tool_button(button: &web_sys::HtmlButtonElement, active: bool) {
    let pressed = if active { "true" } else { "false" };
    let _ = button.set_attribute("aria-pressed", pressed);
}

pub fn set_canvas_mode(canvas: &HtmlCanvasElement, mode: &Mode, dragging: bool) {
    let cursor = match mode {
        Mode::Pan(_) => {
            if dragging {
                "grabbing"
            } else {
                "grab"
            }
        }
        Mode::Erase(_) => "cell",
        Mode::Draw(_) => "crosshair",
        Mode::Select(_) => "default",
    };
    if let Ok(element) = canvas.clone().dyn_into::<HtmlElement>() {
        let _ = element.style().set_property("cursor", cursor);
    }
}

pub fn set_status(status_el: &Element, status_text: &Element, state: &str, text: &str) {
    let _ = status_el.set_attribute("data-state", state);
    status_text.set_text_content(Some(text));
}

pub fn resize_canvas(window: &Window, state: &mut State) {
    let rect = state.canvas.get_bounding_client_rect();
    let dpr = window.device_pixel_ratio();
    state.canvas.set_width((rect.width() * dpr) as u32);
    state.canvas.set_height((rect.height() * dpr) as u32);
    let _ = state.ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
    state.board_width = rect.width();
    state.board_height = rect.height();
    state.board_offset_x = rect.width() / 2.0;
    state.board_offset_y = rect.height() / 2.0;
    redraw(state);
}

pub fn event_to_point(
    canvas: &HtmlCanvasElement,
    event: &PointerEvent,
    pan_x: f64,
    pan_y: f64,
    zoom: f64,
    board_offset_x: f64,
    board_offset_y: f64,
) -> Option<Point> {
    let rect = canvas.get_bounding_client_rect();
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let scale = zoom;
    let x = (event.client_x() as f64 - rect.left() - pan_x - board_offset_x) / scale;
    let y = (event.client_y() as f64 - rect.top() - pan_y - board_offset_y) / scale;
    normalize_point(Point {
        x: x as f32,
        y: y as f32,
    })
}
