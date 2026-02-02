use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use js_sys::{Function, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CanvasRenderingContext2d, Element, Event, FileReader, HtmlAnchorElement, HtmlButtonElement,
    HtmlCanvasElement, HtmlElement, HtmlInputElement, HtmlSpanElement, KeyboardEvent, PointerEvent,
    ProgressEvent,
};

use yumboard_shared::{ClientMessage, ServerMessage, Stroke, TransformOp};

use crate::actions::{
    adopt_strokes, apply_transform_operation, apply_transformed_strokes, clear_board, end_stroke,
    erase_hits_at_point, finalize_lasso_selection, move_stroke, parse_color, remove_stroke,
    replace_stroke_local, restore_stroke, sanitize_size, start_stroke,
};
use crate::dom::{
    event_to_point, get_element, resize_canvas, set_canvas_mode, set_status, set_tool_button,
    update_size_label,
};
use crate::geometry;
use crate::geometry::{
    angle_between, apply_rotation, apply_scale_xy, apply_translation, clamp_scale,
    selected_strokes, selection_center, selection_hit_test,
};
use crate::palette::{palette_action_from_event, render_palette, PaletteAction};
use crate::persistence::{build_pdf_html, open_print_window, parse_load_payload, SaveData};
use crate::render::redraw;
use crate::state::{
    DrawMode, DrawState, EraseMode, LoadingState, Mode, PanMode, PinchState, ScaleAxis, SelectMode,
    SelectState, SelectionHit, State, DEFAULT_PALETTE,
};
use crate::util::make_id;
use crate::ws::{connect_ws, WsEvent};

fn schedule_flush(
    window: &web_sys::Window,
    ws_sender: &Rc<crate::ws::WsSender>,
    state: &Rc<RefCell<State>>,
) {
    let state = state.clone();
    let sender = ws_sender.clone();
    let cb = Closure::once_into_js(move |_: f64| {
        let pending = {
            let mut state = state.borrow_mut();
            state.flush_scheduled = false;
            std::mem::take(&mut state.pending_points)
        };
        for (id, mut points) in pending {
            const MAX_POINTS_PER_MESSAGE: usize = 128;
            while !points.is_empty() {
                let chunk_size = points.len().min(MAX_POINTS_PER_MESSAGE);
                let chunk = points.drain(..chunk_size).collect::<Vec<_>>();
                sender.send(&ClientMessage::StrokePoints {
                    id: id.clone(),
                    points: chunk,
                });
            }
        }
    });
    let _ = window.request_animation_frame(cb.unchecked_ref());
}

fn palette_selected(mode: &Mode) -> Option<usize> {
    match mode {
        Mode::Draw(draw) => Some(draw.palette_selected),
        Mode::Loading(loading) => palette_selected(loading.previous.as_ref()),
        _ => None,
    }
}

fn sync_tool_ui(
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

fn hide_color_input(color_input: &HtmlInputElement) {
    color_input.set_class_name("hidden-color");
}

fn show_color_input(
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

fn coalesced_pointer_events(event: &PointerEvent) -> Vec<PointerEvent> {
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

fn take_loading_previous(state: &mut State) -> Option<Mode> {
    let placeholder = Mode::Pan(PanMode::Idle);
    match std::mem::replace(&mut state.mode, placeholder) {
        Mode::Loading(loading) => {
            let LoadingState {
                previous,
                reader,
                onload,
            } = loading;
            let _ = reader;
            let _ = onload;
            Some(*previous)
        }
        other => {
            state.mode = other;
            None
        }
    }
}

fn is_touch_event(event: &PointerEvent) -> bool {
    event.pointer_type() == "touch"
}

fn pinch_distance(points: &[(f64, f64)]) -> f64 {
    let dx = points[0].0 - points[1].0;
    let dy = points[0].1 - points[1].1;
    (dx * dx + dy * dy).sqrt()
}

fn set_load_busy(load_button: &HtmlButtonElement, busy: bool) {
    let value = if busy { "true" } else { "false" };
    let _ = load_button.set_attribute("aria-busy", value);
}

fn read_load_payload(event: &ProgressEvent) -> Option<Vec<Stroke>> {
    let reader: FileReader = event.target()?.dyn_into().ok()?;
    let text = reader.result().ok()?.as_string()?;
    parse_load_payload(&text)
}

#[wasm_bindgen(start)]
pub fn run() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    start_app()
}

fn start_app() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("Missing window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("Missing document"))?;

    let canvas: HtmlCanvasElement = get_element(&document, "board")?;
    let ctx = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("Missing canvas context"))?
        .dyn_into::<CanvasRenderingContext2d>()?;
    ctx.set_line_cap("round");
    ctx.set_line_join("round");

    let color_input: HtmlInputElement = get_element(&document, "color")?;
    let palette_el: HtmlElement = get_element(&document, "palette")?;
    let size_input: HtmlInputElement = get_element(&document, "size")?;
    let size_value: HtmlSpanElement = get_element(&document, "sizeValue")?;
    let clear_button: HtmlButtonElement = get_element(&document, "clear")?;
    let save_button: HtmlButtonElement = get_element(&document, "save")?;
    let save_menu: HtmlElement = get_element(&document, "saveMenu")?;
    let save_json_button: HtmlButtonElement = get_element(&document, "saveJson")?;
    let save_pdf_button: HtmlButtonElement = get_element(&document, "savePdf")?;
    let load_button: HtmlButtonElement = get_element(&document, "load")?;
    let load_file: HtmlInputElement = get_element(&document, "loadFile")?;
    let lasso_button: HtmlButtonElement = get_element(&document, "lasso")?;
    let eraser_button: HtmlButtonElement = get_element(&document, "eraser")?;
    let pan_button: HtmlButtonElement = get_element(&document, "pan")?;
    let home_button: HtmlButtonElement = get_element(&document, "home")?;
    let undo_button: HtmlButtonElement = get_element(&document, "undo")?;
    let redo_button: HtmlButtonElement = get_element(&document, "redo")?;
    let status_el = document
        .get_element_by_id("status")
        .ok_or_else(|| JsValue::from_str("Missing status element"))?;
    let status_text = document
        .get_element_by_id("statusText")
        .ok_or_else(|| JsValue::from_str("Missing status text"))?;

    let state = Rc::new(RefCell::new(State {
        strokes: Vec::new(),
        active_ids: HashSet::new(),
        board_width: 0.0,
        board_height: 0.0,
        zoom: 1.0,
        pan_x: 0.0,
        pan_y: 0.0,
        palette: DEFAULT_PALETTE
            .iter()
            .map(|value| value.to_string())
            .collect(),
        mode: Mode::Draw(DrawState {
            mode: DrawMode::Idle,
            palette_selected: 0,
        }),
        pending_points: HashMap::new(),
        flush_scheduled: false,
        active_draw_pointer: None,
        active_draw_timestamp: 0.0,
        touch_points: HashMap::new(),
        pinch: None,
        touch_pan: None,
    }));

    update_size_label(&size_input, &size_value);
    set_status(&status_el, &status_text, "connecting", "Connecting...");
    set_tool_button(&lasso_button, false);
    set_tool_button(&eraser_button, false);
    set_tool_button(&pan_button, false);
    set_canvas_mode(&canvas, &state.borrow().mode, false);
    {
        let state = state.borrow();
        let selected = palette_selected(&state.mode);
        if let Some(index) = selected {
            if let Some(color) = state.palette.get(index).cloned() {
                color_input.set_value(&color);
            }
        }
        render_palette(&document, &palette_el, &state.palette, selected);
        show_color_input(&palette_el, &color_input, selected);
    }

    let ws_sender = connect_ws(&window, {
        let status_el = status_el.clone();
        let status_text = status_text.clone();
        let message_state = state.clone();
        let ctx = ctx.clone();
        move |event: WsEvent| match event {
            WsEvent::Open => {
                set_status(&status_el, &status_text, "open", "Live connection");
            }
            WsEvent::Close => {
                set_status(&status_el, &status_text, "closed", "Offline");
            }
            WsEvent::Error => {
                set_status(&status_el, &status_text, "closed", "Connection error");
            }
            WsEvent::Message(message) => {
                let mut state = message_state.borrow_mut();
                match message {
                    ServerMessage::Sync { strokes } => {
                        adopt_strokes(&mut state, &ctx, strokes);
                    }
                    ServerMessage::StrokeStart {
                        id,
                        color,
                        size,
                        point,
                    } => {
                        start_stroke(&mut state, &ctx, id, color, size, point);
                    }
                    ServerMessage::StrokeMove { id, point } => {
                        let _ = move_stroke(&mut state, &ctx, &id, point);
                    }
                    ServerMessage::StrokePoints { id, points } => {
                        for point in points {
                            let _ = move_stroke(&mut state, &ctx, &id, point);
                        }
                    }
                    ServerMessage::StrokeEnd { id } => {
                        end_stroke(&mut state, &id);
                    }
                    ServerMessage::Clear => {
                        clear_board(&mut state, &ctx);
                    }
                    ServerMessage::StrokeRemove { id } => {
                        remove_stroke(&mut state, &id);
                        redraw(&ctx, &mut state);
                    }
                    ServerMessage::StrokeRestore { stroke } => {
                        restore_stroke(&mut state, &ctx, stroke);
                    }
                    ServerMessage::StrokeReplace { stroke } => {
                        replace_stroke_local(&mut state, stroke);
                        redraw(&ctx, &mut state);
                    }
                    ServerMessage::TransformUpdate { ids, op } => {
                        apply_transform_operation(&mut state, &ctx, &ids, &op);
                    }
                }
            }
        }
    })?;

    {
        let resize_state = state.clone();
        let window_cb = window.clone();
        let canvas_cb = canvas.clone();
        let ctx_cb = ctx.clone();
        let onresize = Closure::<dyn FnMut()>::new(move || {
            let mut state = resize_state.borrow_mut();
            resize_canvas(&window_cb, &canvas_cb, &ctx_cb, &mut state);
        });
        window.add_event_listener_with_callback("resize", onresize.as_ref().unchecked_ref())?;
        onresize.forget();
    }

    {
        let key_sender = ws_sender.clone();
        let key_state = state.clone();
        let key_ctx = ctx.clone();
        let onkeydown = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            if !key_sender.is_open() {
                return;
            }
            let key = event.key();
            let modifier = event.meta_key() || event.ctrl_key();
            if !modifier {
                if key == "Delete" || key == "Backspace" {
                    let ids = {
                        let mut state = key_state.borrow_mut();
                        let ids = match &state.mode {
                            Mode::Select(select) => select.selected_ids.clone(),
                            _ => return,
                        };
                        if ids.is_empty() {
                            return;
                        }
                        for id in &ids {
                            remove_stroke(&mut state, id);
                        }
                        if let Mode::Select(select) = &mut state.mode {
                            select.selected_ids.clear();
                            select.mode = SelectMode::Idle;
                        }
                        redraw(&key_ctx, &mut state);
                        ids
                    };
                    key_sender.send(&ClientMessage::Remove { ids });
                    event.prevent_default();
                }
                return;
            }
            if event.shift_key() && key.eq_ignore_ascii_case("z") {
                event.prevent_default();
                key_sender.send(&ClientMessage::Redo);
                return;
            }
            if key.eq_ignore_ascii_case("z") {
                event.prevent_default();
                key_sender.send(&ClientMessage::Undo);
                return;
            }
            if key.eq_ignore_ascii_case("y") {
                event.prevent_default();
                key_sender.send(&ClientMessage::Redo);
            }
        });
        window.add_event_listener_with_callback("keydown", onkeydown.as_ref().unchecked_ref())?;
        onkeydown.forget();
    }

    {
        let mut state = state.borrow_mut();
        resize_canvas(&window, &canvas, &ctx, &mut state);
    }

    {
        let size_input_cb = size_input.clone();
        let size_value_cb = size_value.clone();
        let oninput = Closure::<dyn FnMut(Event)>::new(move |_| {
            update_size_label(&size_input_cb, &size_value_cb);
        });
        size_input.add_event_listener_with_callback("input", oninput.as_ref().unchecked_ref())?;
        oninput.forget();
    }

    {
        let tool_state = state.clone();
        let eraser_button_cb = eraser_button.clone();
        let pan_button_cb = pan_button.clone();
        let lasso_button_cb = lasso_button.clone();
        let palette_el_cb = palette_el.clone();
        let color_input_cb = color_input.clone();
        let canvas = canvas.clone();
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            if matches!(state.mode, Mode::Loading(_)) {
                return;
            }
            state.mode = Mode::Erase(EraseMode::Idle);
            sync_tool_ui(
                &state,
                &canvas,
                &pan_button_cb,
                &eraser_button_cb,
                &lasso_button_cb,
                false,
            );
            render_palette(
                &document,
                &palette_el_cb,
                &state.palette,
                palette_selected(&state.mode),
            );
            hide_color_input(&color_input_cb);
        });
        eraser_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let tool_state = state.clone();
        let eraser_button_cb = eraser_button.clone();
        let pan_button_cb = pan_button.clone();
        let lasso_button_cb = lasso_button.clone();
        let palette_el_cb = palette_el.clone();
        let color_input_cb = color_input.clone();
        let canvas = canvas.clone();
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            if matches!(state.mode, Mode::Loading(_)) {
                return;
            }
            state.mode = Mode::Select(SelectState {
                selected_ids: Vec::new(),
                mode: SelectMode::Idle,
            });
            sync_tool_ui(
                &state,
                &canvas,
                &pan_button_cb,
                &eraser_button_cb,
                &lasso_button_cb,
                false,
            );
            render_palette(
                &document,
                &palette_el_cb,
                &state.palette,
                palette_selected(&state.mode),
            );
            hide_color_input(&color_input_cb);
        });
        lasso_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let tool_state = state.clone();
        let eraser_button_cb = eraser_button.clone();
        let pan_button_cb = pan_button.clone();
        let lasso_button_cb = lasso_button.clone();
        let palette_el_cb = palette_el.clone();
        let color_input_cb = color_input.clone();
        let canvas = canvas.clone();
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            if matches!(state.mode, Mode::Loading(_)) {
                return;
            }
            state.mode = Mode::Pan(PanMode::Idle);
            sync_tool_ui(
                &state,
                &canvas,
                &pan_button_cb,
                &eraser_button_cb,
                &lasso_button_cb,
                false,
            );
            render_palette(
                &document,
                &palette_el_cb,
                &state.palette,
                palette_selected(&state.mode),
            );
            hide_color_input(&color_input_cb);
        });
        pan_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let home_state = state.clone();
        let home_ctx = ctx.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = home_state.borrow_mut();
            if matches!(state.mode, Mode::Loading(_)) {
                return;
            }
            let (zoom, pan_x, pan_y) = geometry::home_zoom_pan(&state);
            state.zoom = zoom;
            state.pan_x = pan_x;
            state.pan_y = pan_y;
            redraw(&home_ctx, &mut state);
        });
        home_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let palette_state = state.clone();
        let palette_el_cb = palette_el.clone();
        let palette_el_listener = palette_el.clone();
        let color_input = color_input.clone();
        let eraser_button_cb = eraser_button.clone();
        let pan_button_cb = pan_button.clone();
        let lasso_button_cb = lasso_button.clone();
        let canvas = canvas.clone();
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let action = match palette_action_from_event(&event) {
                Some(action) => action,
                None => return,
            };
            let mut state = palette_state.borrow_mut();
            if matches!(state.mode, Mode::Loading(_)) {
                return;
            }
            match action {
                PaletteAction::Add => {
                    let color = color_input.value();
                    state.palette.push(color.clone());
                    let palette_selected = state.palette.len().saturating_sub(1);
                    state.mode = Mode::Draw(DrawState {
                        mode: DrawMode::Idle,
                        palette_selected,
                    });
                    color_input.set_value(&color);
                    sync_tool_ui(
                        &state,
                        &canvas,
                        &pan_button_cb,
                        &eraser_button_cb,
                        &lasso_button_cb,
                        false,
                    );
                    render_palette(
                        &document,
                        &palette_el_cb,
                        &state.palette,
                        Some(palette_selected),
                    );
                    show_color_input(&palette_el_cb, &color_input, Some(palette_selected));
                    color_input.click();
                }
                PaletteAction::Select(index) => {
                    if index >= state.palette.len() {
                        return;
                    }
                    let already_selected = palette_selected(&state.mode) == Some(index);
                    state.mode = Mode::Draw(DrawState {
                        mode: DrawMode::Idle,
                        palette_selected: index,
                    });
                    if let Some(color) = state.palette.get(index).cloned() {
                        color_input.set_value(&color);
                    }
                    sync_tool_ui(
                        &state,
                        &canvas,
                        &pan_button_cb,
                        &eraser_button_cb,
                        &lasso_button_cb,
                        false,
                    );
                    render_palette(&document, &palette_el_cb, &state.palette, Some(index));
                    show_color_input(&palette_el_cb, &color_input, Some(index));
                    if already_selected {
                        color_input.click();
                    }
                }
            }
        });
        palette_el_listener
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let palette_state = state.clone();
        let palette_el_cb = palette_el.clone();
        let color_input_cb = color_input.clone();
        let color_input_listener = color_input.clone();
        let document = document.clone();
        let oninput = Closure::<dyn FnMut(Event)>::new(move |_| {
            let color = color_input_cb.value();
            let mut state = palette_state.borrow_mut();
            let selected = match &state.mode {
                Mode::Draw(draw) => draw.palette_selected,
                _ => return,
            };
            if let Some(entry) = state.palette.get_mut(selected) {
                *entry = color;
            } else {
                return;
            }
            render_palette(
                &document,
                &palette_el_cb,
                &state.palette,
                palette_selected(&state.mode),
            );
            show_color_input(
                &palette_el_cb,
                &color_input_cb,
                palette_selected(&state.mode),
            );
        });
        color_input_listener
            .add_event_listener_with_callback("input", oninput.as_ref().unchecked_ref())?;
        oninput.forget();
    }

    {
        let clear_state = state.clone();
        let clear_sender = ws_sender.clone();
        let clear_ctx = ctx.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            if !clear_sender.is_open() {
                return;
            }
            {
                let mut state = clear_state.borrow_mut();
                clear_board(&mut state, &clear_ctx);
            }
            clear_sender.send(&ClientMessage::Clear);
        });
        clear_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let undo_sender = ws_sender.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            if !undo_sender.is_open() {
                return;
            }
            undo_sender.send(&ClientMessage::Undo);
        });
        undo_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let redo_sender = ws_sender.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            if !redo_sender.is_open() {
                return;
            }
            redo_sender.send(&ClientMessage::Redo);
        });
        redo_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let save_menu = save_menu.clone();
        let save_button_cb = save_button.clone();
        let save_button_listener = save_button.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            event.stop_propagation();
            let is_open = !save_menu.has_attribute("hidden");
            if is_open {
                let _ = save_menu.set_attribute("hidden", "");
                let _ = save_button_cb.set_attribute("aria-expanded", "false");
            } else {
                let _ = save_menu.remove_attribute("hidden");
                let _ = save_button_cb.set_attribute("aria-expanded", "true");
            }
        });
        save_button_listener
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let save_state = state.clone();
        let document = document.clone();
        let save_menu = save_menu.clone();
        let save_button = save_button.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let strokes = { save_state.borrow().strokes.clone() };
            let payload = SaveData {
                version: 1,
                strokes,
            };
            let Ok(json) = serde_json::to_string(&payload) else {
                return;
            };
            let encoded = js_sys::encode_uri_component(&json);
            let href = format!("data:application/json;charset=utf-8,{encoded}");
            if let Ok(element) = document.create_element("a") {
                if let Ok(anchor) = element.dyn_into::<HtmlAnchorElement>() {
                    anchor.set_href(&href);
                    anchor.set_download("yumboard.json");
                    anchor.click();
                }
            }
            let _ = save_menu.set_attribute("hidden", "");
            let _ = save_button.set_attribute("aria-expanded", "false");
        });
        save_json_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let save_state = state.clone();
        let document = document.clone();
        let save_menu = save_menu.clone();
        let save_button = save_button.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let html = build_pdf_html(&save_state.borrow(), false);
            open_print_window(&document, &html);
            let _ = save_menu.set_attribute("hidden", "");
            let _ = save_button.set_attribute("aria-expanded", "false");
        });
        save_pdf_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let save_menu = save_menu.clone();
        let save_button = save_button.clone();
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let target: web_sys::EventTarget = match event.target() {
                Some(target) => target,
                None => return,
            };
            let Some(target) = target.dyn_into::<web_sys::Node>().ok() else {
                return;
            };
            let menu_node: web_sys::Node = save_menu.clone().into();
            let button_node: web_sys::Node = save_button.clone().into();
            if menu_node.contains(Some(&target)) || button_node.contains(Some(&target)) {
                return;
            }
            let _ = save_menu.set_attribute("hidden", "");
            let _ = save_button.set_attribute("aria-expanded", "false");
        });
        document.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let load_file = load_file.clone();
        let load_state = state.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            if matches!(load_state.borrow().mode, Mode::Loading(_)) {
                return;
            }
            load_file.set_value("");
            load_file.click();
        });
        load_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let load_file_cb = load_file.clone();
        let load_state_onchange = state.clone();
        let load_sender_onchange = ws_sender.clone();
        let load_button_cb = load_button.clone();
        let load_ctx = ctx.clone();
        let onchange = Closure::<dyn FnMut(Event)>::new(move |_| {
            if !load_sender_onchange.is_open() {
                return;
            }
            let files = load_file_cb.files();
            let file = files.and_then(|list| list.get(0));
            let Some(file) = file else {
                return;
            };
            {
                let state = load_state_onchange.borrow();
                if matches!(state.mode, Mode::Loading(_)) {
                    return;
                }
            }
            let reader = match FileReader::new() {
                Ok(reader) => reader,
                Err(_) => return,
            };
            let load_state_onload = load_state_onchange.clone();
            let load_sender_onload = load_sender_onchange.clone();
            let load_button_onload = load_button_cb.clone();
            let load_ctx = load_ctx.clone();
            let onload = Closure::<dyn FnMut(ProgressEvent)>::new(move |event: ProgressEvent| {
                if !load_sender_onload.is_open() {
                    set_load_busy(&load_button_onload, false);
                    return;
                }
                let strokes = read_load_payload(&event);
                {
                    let mut state = load_state_onload.borrow_mut();
                    let Some(previous) = take_loading_previous(&mut state) else {
                        set_load_busy(&load_button_onload, false);
                        return;
                    };
                    state.mode = previous;
                    if let Some(strokes) = strokes.as_ref() {
                        adopt_strokes(&mut state, &load_ctx, strokes.clone());
                    }
                }
                set_load_busy(&load_button_onload, false);
                if let Some(strokes) = strokes {
                    load_sender_onload.send(&ClientMessage::Load { strokes });
                }
            });
            reader.set_onload(Some(onload.as_ref().unchecked_ref()));
            {
                let mut state = load_state_onchange.borrow_mut();
                let previous = std::mem::replace(&mut state.mode, Mode::Pan(PanMode::Idle));
                state.mode = Mode::Loading(LoadingState {
                    previous: Box::new(previous),
                    reader: None,
                    onload: Some(onload),
                });
            }
            set_load_busy(&load_button_cb, true);
            let _ = reader.read_as_text(&file);
            let mut state = load_state_onchange.borrow_mut();
            if let Mode::Loading(loading) = &mut state.mode {
                loading.reader = Some(reader);
            }
        });
        load_file.add_event_listener_with_callback("change", onchange.as_ref().unchecked_ref())?;
        onchange.forget();
    }

    {
        let down_state = state.clone();
        let down_sender = ws_sender.clone();
        let down_canvas = canvas.clone();
        let down_color = color_input.clone();
        let down_size = size_input.clone();
        let down_ctx = ctx.clone();
        let ondown = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            if event.button() != 0 {
                return;
            }
            event.prevent_default();
            if is_touch_event(&event) {
                let mut state = down_state.borrow_mut();
                state.touch_points.insert(
                    event.pointer_id(),
                    (event.client_x() as f64, event.client_y() as f64),
                );
                if state.touch_points.len() >= 2 {
                    let points = state
                        .touch_points
                        .values()
                        .take(2)
                        .copied()
                        .collect::<Vec<_>>();
                    let center_x = (points[0].0 + points[1].0) / 2.0;
                    let center_y = (points[0].1 + points[1].1) / 2.0;
                    let distance = pinch_distance(&points).max(0.001);
                    let world_center_x = (center_x - state.pan_x) / state.zoom;
                    let world_center_y = (center_y - state.pan_y) / state.zoom;
                    state.pinch = Some(PinchState {
                        world_center_x,
                        world_center_y,
                        distance,
                        zoom: state.zoom,
                    });
                    if let Mode::Draw(draw) = &mut state.mode {
                        if let DrawMode::Drawing { id } = &draw.mode {
                            let id = id.clone();
                            draw.mode = DrawMode::Idle;
                            end_stroke(&mut state, &id);
                            down_sender.send(&ClientMessage::StrokeEnd { id });
                            state.active_draw_pointer = None;
                            state.active_draw_timestamp = 0.0;
                        }
                    }
                    let _ = down_canvas.set_pointer_capture(event.pointer_id());
                    return;
                }
                if state.touch_points.len() == 1 {
                    state.touch_pan = Some(PanMode::Active {
                        start_x: event.client_x() as f64,
                        start_y: event.client_y() as f64,
                        origin_x: state.pan_x,
                        origin_y: state.pan_y,
                    });
                    set_canvas_mode(&down_canvas, &state.mode, true);
                    let _ = down_canvas.set_pointer_capture(event.pointer_id());
                    return;
                }
            }
            let rect = down_canvas.get_bounding_client_rect();
            let screen_x = event.client_x() as f64 - rect.left();
            let screen_y = event.client_y() as f64 - rect.top();
            let (pan_x, pan_y, zoom, select_info) = {
                let state = down_state.borrow();
                let pan_x = state.pan_x;
                let pan_y = state.pan_y;
                let zoom = state.zoom;
                let select_info = match &state.mode {
                    Mode::Select(select) => Some((
                        selection_hit_test(
                            &state.strokes,
                            select,
                            zoom,
                            pan_x,
                            pan_y,
                            screen_x,
                            screen_y,
                        ),
                        select.selected_ids.clone(),
                        selected_strokes(&state.strokes, select),
                        selection_center(&state.strokes, select),
                    )),
                    _ => None,
                };
                (pan_x, pan_y, zoom, select_info)
            };
            let mut state = down_state.borrow_mut();
            let mode = std::mem::replace(&mut state.mode, Mode::Pan(PanMode::Idle));
            match mode {
                Mode::Loading(loading) => {
                    state.mode = Mode::Loading(loading);
                }
                Mode::Select(mut select) => {
                    if !down_sender.is_open() {
                        state.mode = Mode::Select(select);
                        return;
                    }
                    let world_point = match event_to_point(&down_canvas, &event, pan_x, pan_y, zoom)
                    {
                        Some(point) => point,
                        None => {
                            state.mode = Mode::Select(select);
                            return;
                        }
                    };
                    let Some((hit, selection_ids, snapshot, center)) = select_info else {
                        state.mode = Mode::Select(select);
                        return;
                    };
                    if let Some(hit) = hit {
                        match hit {
                            SelectionHit::Trash => {
                                let ids = selection_ids;
                                for id in &ids {
                                    remove_stroke(&mut state, id);
                                }
                                select.selected_ids.clear();
                                select.mode = SelectMode::Idle;
                                state.mode = Mode::Select(select);
                                redraw(&down_ctx, &mut state);
                                down_sender.send(&ClientMessage::Remove { ids });
                                let _ = down_canvas.set_pointer_capture(event.pointer_id());
                                return;
                            }
                            SelectionHit::Rotate => {
                                if let Some(center) = center {
                                    select.mode = SelectMode::Rotate {
                                        center,
                                        start_angle: angle_between(center, world_point),
                                        snapshot,
                                        last_delta: 0.0,
                                    };
                                    let ids = selection_ids.clone();
                                    if !ids.is_empty() {
                                        down_sender.send(&ClientMessage::TransformStart { ids });
                                    }
                                }
                            }
                            SelectionHit::Scale(handle) => {
                                let dx = (world_point.x - handle.anchor.x) as f64;
                                let dy = (world_point.y - handle.anchor.y) as f64;
                                if dx.abs() > f64::EPSILON || dy.abs() > f64::EPSILON {
                                    select.mode = SelectMode::Scale {
                                        anchor: handle.anchor,
                                        start: world_point,
                                        axis: handle.axis,
                                        snapshot,
                                        last_sx: 1.0,
                                        last_sy: 1.0,
                                    };
                                    let ids = selection_ids.clone();
                                    if !ids.is_empty() {
                                        down_sender.send(&ClientMessage::TransformStart { ids });
                                    }
                                }
                            }
                            SelectionHit::Move => {
                                select.mode = SelectMode::Move {
                                    start: world_point,
                                    snapshot,
                                    last_dx: 0.0,
                                    last_dy: 0.0,
                                };
                                let ids = selection_ids.clone();
                                if !ids.is_empty() {
                                    down_sender.send(&ClientMessage::TransformStart { ids });
                                }
                            }
                        }
                        state.mode = Mode::Select(select);
                        let _ = down_canvas.set_pointer_capture(event.pointer_id());
                        return;
                    }
                    select.selected_ids.clear();
                    select.mode = SelectMode::Lasso {
                        points: vec![world_point],
                    };
                    state.mode = Mode::Select(select);
                    redraw(&down_ctx, &mut state);
                    let _ = down_canvas.set_pointer_capture(event.pointer_id());
                }
                Mode::Pan(_) => {
                    state.mode = Mode::Pan(PanMode::Active {
                        start_x: event.client_x() as f64,
                        start_y: event.client_y() as f64,
                        origin_x: pan_x,
                        origin_y: pan_y,
                    });
                    set_canvas_mode(&down_canvas, &state.mode, true);
                    let _ = down_canvas.set_pointer_capture(event.pointer_id());
                }
                Mode::Erase(_) => {
                    if !down_sender.is_open() {
                        state.mode = Mode::Erase(EraseMode::Idle);
                        return;
                    }
                    let point = match event_to_point(&down_canvas, &event, pan_x, pan_y, zoom) {
                        Some(point) => point,
                        None => {
                            state.mode = Mode::Erase(EraseMode::Idle);
                            return;
                        }
                    };
                    state.mode = Mode::Erase(EraseMode::Active {
                        hits: HashSet::new(),
                    });
                    let removed_ids = erase_hits_at_point(&mut state, &down_ctx, point);
                    for id in removed_ids {
                        down_sender.send(&ClientMessage::Erase { id });
                    }
                    let _ = down_canvas.set_pointer_capture(event.pointer_id());
                }
                Mode::Draw(mut draw) => {
                    if !down_sender.is_open() {
                        state.mode = Mode::Draw(draw);
                        return;
                    }
                    let point = match event_to_point(&down_canvas, &event, pan_x, pan_y, zoom) {
                        Some(point) => point,
                        None => {
                            state.mode = Mode::Draw(draw);
                            return;
                        }
                    };
                    let id = make_id();
                    let color = parse_color(&down_color.value());
                    let size = sanitize_size(down_size.value_as_number() as f32);

                    state.active_draw_pointer = Some(event.pointer_id());
                    state.active_draw_timestamp = event.time_stamp();

                    draw.mode = DrawMode::Drawing { id: id.clone() };
                    state.mode = Mode::Draw(draw);
                    start_stroke(
                        &mut state,
                        &down_ctx,
                        id.clone(),
                        color.clone(),
                        size,
                        point,
                    );

                    down_sender.send(&ClientMessage::StrokeStart {
                        id,
                        color,
                        size,
                        point,
                    });
                    let _ = down_canvas.set_pointer_capture(event.pointer_id());
                }
            }
        });
        canvas.add_event_listener_with_callback("pointerdown", ondown.as_ref().unchecked_ref())?;
        ondown.forget();
    }

    {
        let move_state = state.clone();
        let move_sender = ws_sender.clone();
        let move_canvas = canvas.clone();
        let move_window = window.clone();
        let move_ctx = ctx.clone();
        let onmove = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            for event in coalesced_pointer_events(&event) {
                if is_touch_event(&event) {
                    let mut state = move_state.borrow_mut();
                    state.touch_points.insert(
                        event.pointer_id(),
                        (event.client_x() as f64, event.client_y() as f64),
                    );
                    if let Some(pinch) = state.pinch.as_ref() {
                        if state.touch_points.len() >= 2 {
                            let start_distance = pinch.distance;
                            let pinch_zoom = pinch.zoom;
                            let world_center_x = pinch.world_center_x;
                            let world_center_y = pinch.world_center_y;
                            let points = state
                                .touch_points
                                .values()
                                .take(2)
                                .copied()
                                .collect::<Vec<_>>();
                            let center_x = (points[0].0 + points[1].0) / 2.0;
                            let center_y = (points[0].1 + points[1].1) / 2.0;
                            let distance = pinch_distance(&points).max(0.001);
                            let scale = distance / start_distance;
                            let next_zoom = (pinch_zoom * scale).clamp(0.4, 4.0);
                            state.zoom = next_zoom;
                            state.pan_x = center_x - world_center_x * next_zoom;
                            state.pan_y = center_y - world_center_y * next_zoom;
                            redraw(&move_ctx, &mut state);
                            continue;
                        }
                        state.pinch = None;
                    }
                    if state.touch_points.len() == 1 {
                        if let Some(PanMode::Active {
                            start_x,
                            start_y,
                            origin_x,
                            origin_y,
                        }) = state.touch_pan
                        {
                            let next_pan_x = origin_x + (event.client_x() as f64 - start_x);
                            let next_pan_y = origin_y + (event.client_y() as f64 - start_y);
                            state.pan_x = next_pan_x;
                            state.pan_y = next_pan_y;
                            redraw(&move_ctx, &mut state);
                            continue;
                        }
                    }
                }
                let (pan_x, pan_y, zoom) = {
                    let state = move_state.borrow();
                    (state.pan_x, state.pan_y, state.zoom)
                };
                let rect = move_canvas.get_bounding_client_rect();
                let screen_x = event.client_x() as f64 - rect.left();
                let screen_y = event.client_y() as f64 - rect.top();
                let hit = {
                    let state = move_state.borrow();
                    match &state.mode {
                        Mode::Select(select) => selection_hit_test(
                            &state.strokes,
                            select,
                            state.zoom,
                            state.pan_x,
                            state.pan_y,
                            screen_x,
                            screen_y,
                        ),
                        _ => None,
                    }
                };
                let mut state = move_state.borrow_mut();
                match &mut state.mode {
                    Mode::Select(select) => {
                        if !move_sender.is_open() {
                            continue;
                        }
                        let world_point =
                            match event_to_point(&move_canvas, &event, pan_x, pan_y, zoom) {
                                Some(point) => point,
                                None => continue,
                            };
                        let selected_ids = select.selected_ids.clone();
                        let mut pending_update: Option<Vec<Stroke>> = None;
                        let mut pending_message: Option<ClientMessage> = None;
                        match &mut select.mode {
                            SelectMode::Lasso { points } => {
                                points.push(world_point);
                                redraw(&move_ctx, &mut state);
                            }
                            SelectMode::Move {
                                start,
                                snapshot,
                                last_dx,
                                last_dy,
                            } => {
                                let delta_x = world_point.x - start.x;
                                let delta_y = world_point.y - start.y;
                                let updated = apply_translation(snapshot, delta_x, delta_y);
                                let step_dx = delta_x - *last_dx;
                                let step_dy = delta_y - *last_dy;
                                if (step_dx.abs() > f32::EPSILON || step_dy.abs() > f32::EPSILON)
                                    && !selected_ids.is_empty()
                                {
                                    pending_message = Some(ClientMessage::TransformUpdate {
                                        ids: selected_ids.clone(),
                                        op: TransformOp::Translate {
                                            dx: step_dx as f64,
                                            dy: step_dy as f64,
                                        },
                                    });
                                }
                                *last_dx = delta_x;
                                *last_dy = delta_y;
                                pending_update = Some(updated);
                            }
                            SelectMode::Scale {
                                anchor,
                                start,
                                axis,
                                snapshot,
                                last_sx,
                                last_sy,
                            } => {
                                let dx0 = (start.x - anchor.x) as f64;
                                let dy0 = (start.y - anchor.y) as f64;
                                let dx1 = (world_point.x - anchor.x) as f64;
                                let dy1 = (world_point.y - anchor.y) as f64;
                                let (mut sx, mut sy) = match axis {
                                    ScaleAxis::Both => {
                                        let denom = dx0 * dx0 + dy0 * dy0;
                                        let scale = if denom > f64::EPSILON {
                                            (dx1 * dx0 + dy1 * dy0) / denom
                                        } else {
                                            1.0
                                        };
                                        (scale, scale)
                                    }
                                    ScaleAxis::X => {
                                        let scale = if dx0.abs() > f64::EPSILON {
                                            dx1 / dx0
                                        } else {
                                            1.0
                                        };
                                        (scale, 1.0)
                                    }
                                    ScaleAxis::Y => {
                                        let scale = if dy0.abs() > f64::EPSILON {
                                            dy1 / dy0
                                        } else {
                                            1.0
                                        };
                                        (1.0, scale)
                                    }
                                };
                                sx = clamp_scale(sx, 0.05);
                                sy = clamp_scale(sy, 0.05);
                                let updated = apply_scale_xy(snapshot, *anchor, sx, sy);
                                let step_sx = if last_sx.abs() > f64::EPSILON {
                                    sx / *last_sx
                                } else {
                                    sx
                                };
                                let step_sy = if last_sy.abs() > f64::EPSILON {
                                    sy / *last_sy
                                } else {
                                    sy
                                };
                                if (step_sx - 1.0).abs() > f64::EPSILON
                                    || (step_sy - 1.0).abs() > f64::EPSILON
                                {
                                    if !selected_ids.is_empty() {
                                        pending_message = Some(ClientMessage::TransformUpdate {
                                            ids: selected_ids.clone(),
                                            op: TransformOp::Scale {
                                                anchor: *anchor,
                                                sx: step_sx,
                                                sy: step_sy,
                                            },
                                        });
                                    }
                                }
                                *last_sx = sx;
                                *last_sy = sy;
                                pending_update = Some(updated);
                            }
                            SelectMode::Rotate {
                                center,
                                start_angle,
                                snapshot,
                                last_delta,
                            } => {
                                let angle = angle_between(*center, world_point);
                                let delta = angle - *start_angle;
                                let updated = apply_rotation(snapshot, *center, delta);
                                let step_delta = delta - *last_delta;
                                if step_delta.abs() > f64::EPSILON && !selected_ids.is_empty() {
                                    pending_message = Some(ClientMessage::TransformUpdate {
                                        ids: selected_ids.clone(),
                                        op: TransformOp::Rotate {
                                            center: *center,
                                            delta: step_delta,
                                        },
                                    });
                                }
                                *last_delta = delta;
                                pending_update = Some(updated);
                            }
                            SelectMode::Idle => {
                                if hit.is_some() {
                                    set_canvas_mode(&move_canvas, &state.mode, false);
                                }
                            }
                        }
                        if let Some(updated) = pending_update {
                            apply_transformed_strokes(&mut state, &move_ctx, &updated);
                        }
                        if let Some(message) = pending_message {
                            move_sender.send(&message);
                        }
                    }
                    Mode::Erase(EraseMode::Active { .. }) => {
                        if !move_sender.is_open() {
                            continue;
                        }
                        let point = match event_to_point(&move_canvas, &event, pan_x, pan_y, zoom) {
                            Some(point) => point,
                            None => continue,
                        };
                        let removed_ids = erase_hits_at_point(&mut state, &move_ctx, point);
                        for id in removed_ids {
                            move_sender.send(&ClientMessage::Erase { id });
                        }
                    }
                    Mode::Pan(PanMode::Active {
                        start_x,
                        start_y,
                        origin_x,
                        origin_y,
                    }) => {
                        let next_pan_x = *origin_x + (event.client_x() as f64 - *start_x);
                        let next_pan_y = *origin_y + (event.client_y() as f64 - *start_y);
                        state.pan_x = next_pan_x;
                        state.pan_y = next_pan_y;
                        redraw(&move_ctx, &mut state);
                    }
                    Mode::Draw(draw) => {
                        if !move_sender.is_open() {
                            continue;
                        }
                        let id = match &draw.mode {
                            DrawMode::Drawing { id } => id.clone(),
                            _ => continue,
                        };
                        if state.active_draw_pointer != Some(event.pointer_id()) {
                            continue;
                        }
                        let timestamp = event.time_stamp();
                        if timestamp < state.active_draw_timestamp {
                            continue;
                        }
                        state.active_draw_timestamp = timestamp;
                        let point = match event_to_point(&move_canvas, &event, pan_x, pan_y, zoom) {
                            Some(point) => point,
                            None => continue,
                        };
                        if move_stroke(&mut state, &move_ctx, &id, point) {
                            state.pending_points.entry(id).or_default().push(point);
                            let should_schedule = if state.flush_scheduled {
                                false
                            } else {
                                state.flush_scheduled = true;
                                true
                            };
                            if should_schedule {
                                drop(state);
                                schedule_flush(&move_window, &move_sender, &move_state);
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
        canvas.add_event_listener_with_callback("pointermove", onmove.as_ref().unchecked_ref())?;
        canvas.add_event_listener_with_callback(
            "pointerrawupdate",
            onmove.as_ref().unchecked_ref(),
        )?;
        onmove.forget();
    }

    {
        let stop_state = state.clone();
        let stop_sender = ws_sender.clone();
        let stop_canvas = canvas.clone();
        let stop_ctx = ctx.clone();
        let onstop = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            let mut state = stop_state.borrow_mut();
            if is_touch_event(&event) {
                state.touch_points.remove(&event.pointer_id());
                if state.touch_points.len() < 2 {
                    state.pinch = None;
                }
                if state.touch_points.is_empty() {
                    state.touch_pan = None;
                }
                event.prevent_default();
                if stop_canvas.has_pointer_capture(event.pointer_id()) {
                    let _ = stop_canvas.release_pointer_capture(event.pointer_id());
                }
                return;
            }
            let active = matches!(
                &state.mode,
                Mode::Select(_)
                    | Mode::Erase(EraseMode::Active { .. })
                    | Mode::Pan(PanMode::Active { .. })
                    | Mode::Draw(DrawState {
                        mode: DrawMode::Drawing { .. },
                        ..
                    })
            );
            if !active {
                return;
            }
            event.prevent_default();
            if stop_canvas.has_pointer_capture(event.pointer_id()) {
                let _ = stop_canvas.release_pointer_capture(event.pointer_id());
            }
            let mode = std::mem::replace(&mut state.mode, Mode::Pan(PanMode::Idle));
            match mode {
                Mode::Select(select) => {
                    let end_ids = match select.mode {
                        SelectMode::Move { .. }
                        | SelectMode::Scale { .. }
                        | SelectMode::Rotate { .. } => Some(select.selected_ids.clone()),
                        _ => None,
                    };
                    state.mode = Mode::Select(select);
                    if matches!(
                        state.mode,
                        Mode::Select(SelectState {
                            mode: SelectMode::Lasso { .. },
                            ..
                        })
                    ) {
                        finalize_lasso_selection(&mut state);
                    }
                    if let Mode::Select(select) = &mut state.mode {
                        select.mode = SelectMode::Idle;
                    }
                    redraw(&stop_ctx, &mut state);
                    drop(state);
                    if let Some(ids) = end_ids {
                        if !ids.is_empty() {
                            stop_sender.send(&ClientMessage::TransformEnd { ids });
                        }
                    }
                }
                Mode::Erase(EraseMode::Active { .. }) => {
                    state.mode = Mode::Erase(EraseMode::Idle);
                }
                Mode::Pan(PanMode::Active { .. }) => {
                    state.mode = Mode::Pan(PanMode::Idle);
                    set_canvas_mode(&stop_canvas, &state.mode, false);
                }
                Mode::Draw(mut draw) => {
                    if state.active_draw_pointer != Some(event.pointer_id()) {
                        state.mode = Mode::Draw(draw);
                        return;
                    }
                    state.active_draw_pointer = None;
                    state.active_draw_timestamp = 0.0;
                    let id = match &draw.mode {
                        DrawMode::Drawing { id } => id.clone(),
                        _ => {
                            state.mode = Mode::Draw(draw);
                            return;
                        }
                    };
                    draw.mode = DrawMode::Idle;
                    state.mode = Mode::Draw(draw);
                    end_stroke(&mut state, &id);
                    if let Some(mut points) = state.pending_points.remove(&id) {
                        drop(state);
                        const MAX_POINTS_PER_MESSAGE: usize = 128;
                        while !points.is_empty() {
                            let chunk_size = points.len().min(MAX_POINTS_PER_MESSAGE);
                            let chunk = points.drain(..chunk_size).collect::<Vec<_>>();
                            stop_sender.send(&ClientMessage::StrokePoints {
                                id: id.clone(),
                                points: chunk,
                            });
                        }
                    } else {
                        drop(state);
                    }
                    stop_sender.send(&ClientMessage::StrokeEnd { id });
                }
                other => {
                    state.mode = other;
                }
            }
        });
        canvas.add_event_listener_with_callback("pointerup", onstop.as_ref().unchecked_ref())?;
        canvas
            .add_event_listener_with_callback("pointercancel", onstop.as_ref().unchecked_ref())?;
        canvas.add_event_listener_with_callback("pointerleave", onstop.as_ref().unchecked_ref())?;
        canvas.add_event_listener_with_callback(
            "lostpointercapture",
            onstop.as_ref().unchecked_ref(),
        )?;
        onstop.forget();
    }

    {
        let zoom_state = state.clone();
        let zoom_canvas = canvas.clone();
        let zoom_ctx = ctx.clone();
        let onwheel = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let wheel_event = match event.dyn_into::<web_sys::WheelEvent>() {
                Ok(event) => event,
                Err(_) => return,
            };
            wheel_event.prevent_default();
            let rect = zoom_canvas.get_bounding_client_rect();
            let (zoom, pan_x, pan_y) = {
                let state = zoom_state.borrow();
                (state.zoom, state.pan_x, state.pan_y)
            };
            let cursor_x = wheel_event.client_x() as f64 - rect.left();
            let cursor_y = wheel_event.client_y() as f64 - rect.top();
            let world_x = (cursor_x - pan_x) / zoom;
            let world_y = (cursor_y - pan_y) / zoom;

            const UNIT_SCROLL: f64 = 200.0;
            let zoom_factor = (wheel_event.delta_y() / UNIT_SCROLL).exp();
            let next_zoom = zoom * zoom_factor;
            let next_pan_x = cursor_x - world_x * next_zoom;
            let next_pan_y = cursor_y - world_y * next_zoom;
            {
                let mut state = zoom_state.borrow_mut();
                state.zoom = next_zoom;
                state.pan_x = next_pan_x;
                state.pan_y = next_pan_y;
                redraw(&zoom_ctx, &mut state);
            }
        });
        canvas.add_event_listener_with_callback("wheel", onwheel.as_ref().unchecked_ref())?;
        onwheel.forget();
    }

    Ok(())
}
