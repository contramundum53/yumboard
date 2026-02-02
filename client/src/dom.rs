use js_sys::{Function, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    Document, Element, HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement,
    HtmlSpanElement, PointerEvent, Window,
};

use yumboard_shared::Point;

use crate::geometry;
use crate::geometry::normalize_point;
use crate::render::redraw;
use crate::state::{Mode, State};

pub struct Ui {
    pub document: Document,
    pub canvas: HtmlCanvasElement,
    pub color_input: HtmlInputElement,
    pub palette_el: HtmlElement,
    pub size_input: HtmlInputElement,
    pub size_value: HtmlSpanElement,
    pub clear_button: HtmlButtonElement,
    pub save_button: HtmlButtonElement,
    pub save_menu: HtmlElement,
    pub save_json_button: HtmlButtonElement,
    pub save_pdf_button: HtmlButtonElement,
    pub load_button: HtmlButtonElement,
    pub load_file: HtmlInputElement,
    pub lasso_button: HtmlButtonElement,
    pub eraser_button: HtmlButtonElement,
    pub pan_button: HtmlButtonElement,
    pub home_button: HtmlButtonElement,
    pub undo_button: HtmlButtonElement,
    pub redo_button: HtmlButtonElement,
    pub status_el: Element,
    pub status_text: Element,
}

impl Ui {
    pub fn from_document(document: Document) -> Result<Self, JsValue> {
        let canvas: HtmlCanvasElement = get_element(&document, "board")?;
        Ok(Self {
            color_input: get_element(&document, "color")?,
            palette_el: get_element(&document, "palette")?,
            size_input: get_element(&document, "size")?,
            size_value: get_element(&document, "sizeValue")?,
            clear_button: get_element(&document, "clear")?,
            save_button: get_element(&document, "save")?,
            save_menu: get_element(&document, "saveMenu")?,
            save_json_button: get_element(&document, "saveJson")?,
            save_pdf_button: get_element(&document, "savePdf")?,
            load_button: get_element(&document, "load")?,
            load_file: get_element(&document, "loadFile")?,
            lasso_button: get_element(&document, "lasso")?,
            eraser_button: get_element(&document, "eraser")?,
            pan_button: get_element(&document, "pan")?,
            home_button: get_element(&document, "home")?,
            undo_button: get_element(&document, "undo")?,
            redo_button: get_element(&document, "redo")?,
            status_el: document
                .get_element_by_id("status")
                .ok_or_else(|| JsValue::from_str("Missing status element"))?,
            status_text: document
                .get_element_by_id("statusText")
                .ok_or_else(|| JsValue::from_str("Missing status text"))?,
            document,
            canvas,
        })
    }
}

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
        Mode::Loading(_) => "progress",
    };
    if let Ok(element) = canvas.clone().dyn_into::<HtmlElement>() {
        let _ = element.style().set_property("cursor", cursor);
    }
}

pub fn sync_tool_ui(
    state: &State,
    canvas: &HtmlCanvasElement,
    pan_button: &HtmlButtonElement,
    eraser_button: &HtmlButtonElement,
    lasso_button: &HtmlButtonElement,
    dragging: bool,
) {
    let is_pan = matches!(state.mode, Mode::Pan(_));
    let is_erase = matches!(state.mode, Mode::Erase(_));
    let is_select = matches!(state.mode, Mode::Select(_));
    set_tool_button(pan_button, is_pan);
    set_tool_button(eraser_button, is_erase);
    set_tool_button(lasso_button, is_select);
    set_canvas_mode(canvas, &state.mode, dragging);
}

pub fn hide_color_input(color_input: &HtmlInputElement) {
    color_input.set_class_name("hidden-color");
}

pub fn show_color_input(
    palette_el: &HtmlElement,
    color_input: &HtmlInputElement,
    selected: Option<usize>,
) {
    let Some(index) = selected else {
        hide_color_input(color_input);
        return;
    };
    let selector = format!("[data-index=\"{index}\"]");
    let Ok(Some(node)) = palette_el.query_selector(&selector) else {
        hide_color_input(color_input);
        return;
    };
    let rect = node.get_bounding_client_rect();
    let toolbar_rect = palette_el
        .closest(".toolbar")
        .ok()
        .flatten()
        .map(|toolbar: Element| toolbar.get_bounding_client_rect());
    let style = color_input.style();
    let (left, top) = if let Some(toolbar_rect) = toolbar_rect {
        (
            rect.left() - toolbar_rect.left(),
            rect.top() - toolbar_rect.top(),
        )
    } else {
        (rect.left(), rect.top())
    };
    let _ = style.set_property("left", &format!("{}px", left));
    let _ = style.set_property("top", &format!("{}px", top));
    let _ = style.set_property("width", &format!("{}px", rect.width()));
    let _ = style.set_property("height", &format!("{}px", rect.height()));
    color_input.set_class_name("hidden-color active");
}

pub fn coalesced_pointer_events(event: &PointerEvent) -> Vec<PointerEvent> {
    let get_coalesced_events =
        Reflect::get(event.as_ref(), &JsValue::from_str("getCoalescedEvents"))
            .ok()
            .and_then(|value| value.dyn_into::<Function>().ok());

    let mut out = Vec::new();
    if let Some(get_coalesced_events) = get_coalesced_events {
        if let Ok(events) = get_coalesced_events
            .call0(event.as_ref())
            .and_then(|value| value.dyn_into::<js_sys::Array>())
        {
            out.reserve(events.length() as usize + 1);
            for index in 0..events.length() {
                if let Ok(event) = events.get(index).dyn_into::<PointerEvent>() {
                    out.push(event);
                }
            }
        }
    }
    out.push(event.clone());
    out.sort_by(|a, b| {
        a.time_stamp()
            .partial_cmp(&b.time_stamp())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

pub fn is_touch_event(event: &PointerEvent) -> bool {
    event.pointer_type() == "touch"
}

pub fn set_load_busy(load_button: &HtmlButtonElement, busy: bool) {
    let value = if busy { "true" } else { "false" };
    let _ = load_button.set_attribute("aria-busy", value);
}

pub fn set_status(status_el: &Element, status_text: &Element, state: &str, text: &str) {
    let _ = status_el.set_attribute("data-state", state);
    status_text.set_text_content(Some(text));
}

pub fn resize_canvas(
    window: &Window,
    canvas: &HtmlCanvasElement,
    ctx: &web_sys::CanvasRenderingContext2d,
    state: &mut State,
) {
    let last_board_width = state.board_width;
    let last_board_height = state.board_height;
    web_sys::console::log_1(
        &format!(
            "Resizing canvas from {}x{}",
            last_board_width, last_board_height
        )
        .into(),
    );

    let rect = canvas.get_bounding_client_rect();
    let dpr = window.device_pixel_ratio();
    canvas.set_width((rect.width() * dpr) as u32);
    canvas.set_height((rect.height() * dpr) as u32);
    let _ = ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
    state.board_width = rect.width();
    state.board_height = rect.height();

    if last_board_width == 0.0 || last_board_height == 0.0 {
        web_sys::console::log_1(&"Initial canvas size, resetting to home view".into());
        let (zoom, pan_x, pan_y) = geometry::home_zoom_pan(&state);

        state.zoom = zoom;
        state.pan_x = pan_x;
        state.pan_y = pan_y;
    } else {
        state.pan_x += (state.board_width - last_board_width) / 2.0;
        state.pan_y += (state.board_height - last_board_height) / 2.0;
    }
    redraw(ctx, state);
}

pub fn event_to_point(
    canvas: &HtmlCanvasElement,
    event: &PointerEvent,
    pan_x: f64,
    pan_y: f64,
    zoom: f64,
) -> Option<Point> {
    let rect = canvas.get_bounding_client_rect();
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let scale = zoom;
    let x = (event.client_x() as f64 - rect.left() - pan_x) / scale;
    let y = (event.client_y() as f64 - rect.top() - pan_y) / scale;
    normalize_point(Point {
        x: x as f32,
        y: y as f32,
    })
}
