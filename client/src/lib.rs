use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CanvasRenderingContext2d, Document, Element, Event, HtmlButtonElement, HtmlCanvasElement,
    HtmlElement, HtmlInputElement, HtmlSpanElement, KeyboardEvent, MessageEvent, PointerEvent,
    WebSocket, Window,
};

use pfboard_shared::{ClientMessage, Point, ServerMessage, Stroke};

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Draw,
    Erase,
    Pan,
}

struct State {
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    strokes: Vec<Stroke>,
    active_ids: HashSet<String>,
    board_width: f64,
    board_height: f64,
    board_scale: f64,
    board_offset_x: f64,
    board_offset_y: f64,
    zoom: f64,
    current_id: Option<String>,
    drawing: bool,
    erasing: bool,
    tool: Tool,
    erase_hits: HashSet<String>,
    panning: bool,
    pan_start_x: f64,
    pan_start_y: f64,
    pan_origin_x: f64,
    pan_origin_y: f64,
    pan_x: f64,
    pan_y: f64,
}

#[wasm_bindgen(start)]
pub fn run() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

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
    let size_input: HtmlInputElement = get_element(&document, "size")?;
    let size_value: HtmlSpanElement = get_element(&document, "sizeValue")?;
    let clear_button: HtmlButtonElement = get_element(&document, "clear")?;
    let pen_button: HtmlButtonElement = get_element(&document, "pen")?;
    let eraser_button: HtmlButtonElement = get_element(&document, "eraser")?;
    let pan_button: HtmlButtonElement = get_element(&document, "pan")?;
    let status_el = document
        .get_element_by_id("status")
        .ok_or_else(|| JsValue::from_str("Missing status element"))?;
    let status_text = document
        .get_element_by_id("statusText")
        .ok_or_else(|| JsValue::from_str("Missing status text"))?;

    let state = Rc::new(RefCell::new(State {
        canvas: canvas.clone(),
        ctx,
        strokes: Vec::new(),
        active_ids: HashSet::new(),
        board_width: 0.0,
        board_height: 0.0,
        board_scale: 0.0,
        board_offset_x: 0.0,
        board_offset_y: 0.0,
        zoom: 1.0,
        current_id: None,
        drawing: false,
        erasing: false,
        tool: Tool::Draw,
        erase_hits: HashSet::new(),
        panning: false,
        pan_start_x: 0.0,
        pan_start_y: 0.0,
        pan_origin_x: 0.0,
        pan_origin_y: 0.0,
        pan_x: 0.0,
        pan_y: 0.0,
    }));

    update_size_label(&size_input, &size_value);
    set_status(&status_el, &status_text, "connecting", "Connecting...");
    set_tool_button(&pen_button, true);
    set_tool_button(&eraser_button, false);
    set_tool_button(&pan_button, false);
    set_canvas_mode(&canvas, Tool::Draw, false);

    let ws_url = websocket_url(&window)?;
    let socket = Rc::new(WebSocket::new(&ws_url)?);

    {
        let status_el = status_el.clone();
        let status_text = status_text.clone();
        let onopen = Closure::<dyn FnMut(Event)>::new(move |_| {
            set_status(&status_el, &status_text, "open", "Live connection");
        });
        socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();
    }

    {
        let status_el = status_el.clone();
        let status_text = status_text.clone();
        let onclose = Closure::<dyn FnMut(Event)>::new(move |_| {
            set_status(&status_el, &status_text, "closed", "Offline");
        });
        socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();
    }

    {
        let status_el = status_el.clone();
        let status_text = status_text.clone();
        let onerror = Closure::<dyn FnMut(Event)>::new(move |_| {
            set_status(&status_el, &status_text, "closed", "Connection error");
        });
        socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();
    }

    {
        let message_state = state.clone();
        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let text = match event.data().as_string() {
                Some(text) => text,
                None => return,
            };
            let message = match serde_json::from_str::<ServerMessage>(&text) {
                Ok(message) => message,
                Err(_) => return,
            };

            let mut state = message_state.borrow_mut();
            match message {
                ServerMessage::Sync { strokes } => {
                    adopt_strokes(&mut state, strokes);
                }
                ServerMessage::StrokeStart {
                    id,
                    color,
                    size,
                    point,
                } => {
                    start_stroke(&mut state, id, color, size, point);
                }
                ServerMessage::StrokeMove { id, point } => {
                    move_stroke(&mut state, &id, point);
                }
                ServerMessage::StrokeEnd { id } => {
                    end_stroke(&mut state, &id);
                }
                ServerMessage::Clear => {
                    clear_board(&mut state);
                }
                ServerMessage::StrokeRemove { id } => {
                    remove_stroke(&mut state, &id);
                }
                ServerMessage::StrokeRestore { stroke } => {
                    restore_stroke(&mut state, stroke);
                }
            }
        });
        socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
    }

    {
        let resize_state = state.clone();
        let window_cb = window.clone();
        let onresize = Closure::<dyn FnMut()>::new(move || {
            let mut state = resize_state.borrow_mut();
            resize_canvas(&window_cb, &mut state);
        });
        window.add_event_listener_with_callback("resize", onresize.as_ref().unchecked_ref())?;
        onresize.forget();
    }

    {
        let key_socket = socket.clone();
        let onkeydown = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            let key = event.key();
            let modifier = event.meta_key() || event.ctrl_key();
            if !modifier {
                return;
            }
            if event.shift_key() && key.eq_ignore_ascii_case("z") {
                event.prevent_default();
                send_message(&key_socket, &ClientMessage::Redo);
                return;
            }
            if key.eq_ignore_ascii_case("z") {
                event.prevent_default();
                send_message(&key_socket, &ClientMessage::Undo);
                return;
            }
            if key.eq_ignore_ascii_case("y") {
                event.prevent_default();
                send_message(&key_socket, &ClientMessage::Redo);
            }
        });
        window.add_event_listener_with_callback("keydown", onkeydown.as_ref().unchecked_ref())?;
        onkeydown.forget();
    }

    {
        let mut state = state.borrow_mut();
        resize_canvas(&window, &mut state);
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
        let pen_button_cb = pen_button.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            state.tool = Tool::Erase;
            state.drawing = false;
            state.current_id = None;
            state.erasing = false;
            state.panning = false;
            state.erase_hits.clear();
            set_tool_button(&pan_button_cb, state.tool == Tool::Pan);
            set_tool_button(&eraser_button_cb, state.tool == Tool::Erase);
            set_tool_button(&pen_button_cb, state.tool == Tool::Draw);
            set_canvas_mode(&state.canvas, state.tool, false);
        });
        eraser_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let tool_state = state.clone();
        let eraser_button_cb = eraser_button.clone();
        let pan_button_cb = pan_button.clone();
        let pen_button_cb = pen_button.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            state.tool = Tool::Draw;
            state.drawing = false;
            state.current_id = None;
            state.erasing = false;
            state.panning = false;
            state.erase_hits.clear();
            set_tool_button(&eraser_button_cb, state.tool == Tool::Erase);
            set_tool_button(&pan_button_cb, state.tool == Tool::Pan);
            set_tool_button(&pen_button_cb, state.tool == Tool::Draw);
            set_canvas_mode(&state.canvas, state.tool, false);
        });
        pen_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let tool_state = state.clone();
        let eraser_button_cb = eraser_button.clone();
        let pan_button_cb = pan_button.clone();
        let pen_button_cb = pen_button.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            state.tool = if state.tool == Tool::Pan {
                Tool::Draw
            } else {
                Tool::Pan
            };
            state.drawing = false;
            state.current_id = None;
            state.erasing = false;
            state.panning = false;
            state.erase_hits.clear();
            set_tool_button(&eraser_button_cb, state.tool == Tool::Erase);
            set_tool_button(&pan_button_cb, state.tool == Tool::Pan);
            set_tool_button(&pen_button_cb, state.tool == Tool::Draw);
            set_canvas_mode(&state.canvas, state.tool, false);
        });
        pan_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let clear_state = state.clone();
        let clear_socket = socket.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            {
                let mut state = clear_state.borrow_mut();
                clear_board(&mut state);
            }
            send_message(&clear_socket, &ClientMessage::Clear);
        });
        clear_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let down_state = state.clone();
        let down_socket = socket.clone();
        let down_canvas = canvas.clone();
        let down_color = color_input.clone();
        let down_size = size_input.clone();
        let ondown = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            if event.button() != 0 {
                return;
            }
            event.prevent_default();
            let tool = { down_state.borrow().tool };
            if tool == Tool::Pan {
                let mut state = down_state.borrow_mut();
                state.panning = true;
                state.pan_start_x = event.client_x() as f64;
                state.pan_start_y = event.client_y() as f64;
                state.pan_origin_x = state.pan_x;
                state.pan_origin_y = state.pan_y;
                set_canvas_mode(&state.canvas, Tool::Pan, true);
                let _ = down_canvas.set_pointer_capture(event.pointer_id());
                return;
            }
            let (pan_x, pan_y, zoom) = {
                let state = down_state.borrow();
                (state.pan_x, state.pan_y, state.zoom)
            };
            let point = match event_to_point(&down_canvas, &event, pan_x, pan_y, zoom) {
                Some(point) => point,
                None => return,
            };
            if tool == Tool::Erase {
                let removed_ids = {
                    let mut state = down_state.borrow_mut();
                    state.erasing = true;
                    state.erase_hits.clear();
                    erase_hits_at_point(&mut state, point)
                };
                for id in removed_ids {
                    send_message(&down_socket, &ClientMessage::Erase { id });
                }
                let _ = down_canvas.set_pointer_capture(event.pointer_id());
                return;
            }
            let id = make_id();
            let color = down_color.value();
            let size = sanitize_size(down_size.value_as_number() as f32);

            {
                let mut state = down_state.borrow_mut();
                state.drawing = true;
                state.current_id = Some(id.clone());
                start_stroke(&mut state, id.clone(), color.clone(), size, point);
            }

            send_message(
                &down_socket,
                &ClientMessage::StrokeStart {
                    id,
                    color,
                    size,
                    point,
                },
            );
            let _ = down_canvas.set_pointer_capture(event.pointer_id());
        });
        canvas.add_event_listener_with_callback("pointerdown", ondown.as_ref().unchecked_ref())?;
        ondown.forget();
    }

    {
        let move_state = state.clone();
        let move_socket = socket.clone();
        let move_canvas = canvas.clone();
        let onmove = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            let tool = { move_state.borrow().tool };
            if tool == Tool::Erase {
                let (pan_x, pan_y, zoom) = {
                    let state = move_state.borrow();
                    (state.pan_x, state.pan_y, state.zoom)
                };
                let point = match event_to_point(&move_canvas, &event, pan_x, pan_y, zoom) {
                    Some(point) => point,
                    None => return,
                };
                let removed_ids = {
                    let mut state = move_state.borrow_mut();
                    if !state.erasing {
                        return;
                    }
                    erase_hits_at_point(&mut state, point)
                };
                for id in removed_ids {
                    send_message(&move_socket, &ClientMessage::Erase { id });
                }
                return;
            }

            let tool = { move_state.borrow().tool };
            if tool == Tool::Pan {
                let (pan_start_x, pan_start_y, pan_origin_x, pan_origin_y) = {
                    let state = move_state.borrow();
                    if !state.panning {
                        return;
                    }
                    (
                        state.pan_start_x,
                        state.pan_start_y,
                        state.pan_origin_x,
                        state.pan_origin_y,
                    )
                };
                let next_pan_x = pan_origin_x + (event.client_x() as f64 - pan_start_x);
                let next_pan_y = pan_origin_y + (event.client_y() as f64 - pan_start_y);
                {
                    let mut state = move_state.borrow_mut();
                    state.pan_x = next_pan_x;
                    state.pan_y = next_pan_y;
                    redraw(&mut state);
                }
                return;
            }
            let (pan_x, pan_y, zoom) = {
                let state = move_state.borrow();
                (state.pan_x, state.pan_y, state.zoom)
            };
            let (id, point, last_point) = {
                let state = move_state.borrow();
                if !state.drawing {
                    return;
                }
                let id = match state.current_id.clone() {
                    Some(id) => id,
                    None => return,
                };
                let point = match event_to_point(&move_canvas, &event, pan_x, pan_y, zoom) {
                    Some(point) => point,
                    None => return,
                };
                let last_point = last_point_for_id(&state.strokes, &id);
                (id, point, last_point)
            };

            if let Some(last_point) = last_point {
                if last_point == point {
                    return;
                }
            }

            {
                let mut state = move_state.borrow_mut();
                move_stroke(&mut state, &id, point);
            }

            send_message(&move_socket, &ClientMessage::StrokeMove { id, point });
        });
        canvas.add_event_listener_with_callback("pointermove", onmove.as_ref().unchecked_ref())?;
        onmove.forget();
    }

    {
        let stop_state = state.clone();
        let stop_socket = socket.clone();
        let stop_canvas = canvas.clone();
        let onstop = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            let mode = {
                let state = stop_state.borrow();
                if state.drawing {
                    Some(Tool::Draw)
                } else if state.erasing {
                    Some(Tool::Erase)
                } else if state.panning {
                    Some(Tool::Pan)
                } else {
                    None
                }
            };

            let Some(mode) = mode else {
                return;
            };

            event.prevent_default();
            if stop_canvas.has_pointer_capture(event.pointer_id()) {
                let _ = stop_canvas.release_pointer_capture(event.pointer_id());
            }

            if mode == Tool::Erase {
                let mut state = stop_state.borrow_mut();
                state.erasing = false;
                state.erase_hits.clear();
                return;
            }
            if mode == Tool::Pan {
                let mut state = stop_state.borrow_mut();
                state.panning = false;
                set_canvas_mode(&state.canvas, Tool::Pan, false);
                return;
            }

            let id = {
                let state = stop_state.borrow();
                state.current_id.clone()
            };

            let Some(id) = id else {
                return;
            };

            {
                let mut state = stop_state.borrow_mut();
                state.drawing = false;
                state.current_id = None;
                end_stroke(&mut state, &id);
            }

            send_message(&stop_socket, &ClientMessage::StrokeEnd { id });
        });
        canvas.add_event_listener_with_callback("pointerup", onstop.as_ref().unchecked_ref())?;
        canvas
            .add_event_listener_with_callback("pointercancel", onstop.as_ref().unchecked_ref())?;
        canvas.add_event_listener_with_callback("pointerleave", onstop.as_ref().unchecked_ref())?;
        onstop.forget();
    }

    {
        let zoom_state = state.clone();
        let zoom_canvas = canvas.clone();
        let onwheel = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let wheel_event = match event.dyn_into::<web_sys::WheelEvent>() {
                Ok(event) => event,
                Err(_) => return,
            };
            wheel_event.prevent_default();
            let rect = zoom_canvas.get_bounding_client_rect();
            let (offset_x, offset_y, board_scale, zoom, pan_x, pan_y) = {
                let state = zoom_state.borrow();
                (
                    state.board_offset_x,
                    state.board_offset_y,
                    state.board_scale,
                    state.zoom,
                    state.pan_x,
                    state.pan_y,
                )
            };
            if board_scale <= 0.0 {
                return;
            }
            let scale = board_scale * zoom;
            let cursor_x = wheel_event.client_x() as f64 - rect.left();
            let cursor_y = wheel_event.client_y() as f64 - rect.top();
            let world_x = (cursor_x - pan_x - offset_x) / scale;
            let world_y = (cursor_y - pan_y - offset_y) / scale;
            let zoom_factor = if wheel_event.delta_y() < 0.0 { 1.1 } else { 0.9 };
            let next_zoom = (zoom * zoom_factor).clamp(0.4, 4.0);
            let next_scale = board_scale * next_zoom;
            let next_pan_x = cursor_x - offset_x - world_x * next_scale;
            let next_pan_y = cursor_y - offset_y - world_y * next_scale;
            {
                let mut state = zoom_state.borrow_mut();
                state.zoom = next_zoom;
                state.pan_x = next_pan_x;
                state.pan_y = next_pan_y;
                redraw(&mut state);
            }
        });
        canvas.add_event_listener_with_callback("wheel", onwheel.as_ref().unchecked_ref())?;
        onwheel.forget();
    }

    Ok(())
}

fn get_element<T: JsCast>(document: &Document, id: &str) -> Result<T, JsValue> {
    let element = document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("Missing element: {id}")))?;
    element
        .dyn_into::<T>()
        .map_err(|_| JsValue::from_str(&format!("Invalid element type: {id}")))
}

fn websocket_url(window: &Window) -> Result<String, JsValue> {
    let location = window.location();
    let protocol = location.protocol()?;
    let host = location.host()?;
    let scheme = if protocol == "https:" { "wss" } else { "ws" };
    Ok(format!("{scheme}://{host}/ws"))
}

fn update_size_label(input: &HtmlInputElement, value: &HtmlSpanElement) {
    value.set_text_content(Some(&input.value()));
}

fn set_tool_button(button: &HtmlButtonElement, active: bool) {
    let pressed = if active { "true" } else { "false" };
    let _ = button.set_attribute("aria-pressed", pressed);
}

fn set_canvas_mode(canvas: &HtmlCanvasElement, tool: Tool, dragging: bool) {
    let cursor = match tool {
        Tool::Pan => {
            if dragging {
                "grabbing"
            } else {
                "grab"
            }
        }
        Tool::Erase => "cell",
        Tool::Draw => "crosshair",
    };
    if let Ok(element) = canvas.clone().dyn_into::<HtmlElement>() {
        let _ = element.style().set_property("cursor", cursor);
    }
}

fn set_status(status_el: &Element, status_text: &Element, state: &str, text: &str) {
    let _ = status_el.set_attribute("data-state", state);
    status_text.set_text_content(Some(text));
}

fn resize_canvas(window: &Window, state: &mut State) {
    let rect = state.canvas.get_bounding_client_rect();
    let dpr = window.device_pixel_ratio();
    state.canvas.set_width((rect.width() * dpr) as u32);
    state.canvas.set_height((rect.height() * dpr) as u32);
    let _ = state.ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
    state.board_width = rect.width();
    state.board_height = rect.height();
    state.board_scale = rect.width().min(rect.height());
    state.board_offset_x = (rect.width() - state.board_scale) / 2.0;
    state.board_offset_y = (rect.height() - state.board_scale) / 2.0;
    redraw(state);
}

fn event_to_point(
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
    let scale = rect.width().min(rect.height());
    if scale <= 0.0 {
        return None;
    }
    let scale = scale * zoom;
    let offset_x = (rect.width() - scale) / 2.0;
    let offset_y = (rect.height() - scale) / 2.0;
    let x = (event.client_x() as f64 - rect.left() - pan_x - offset_x) / scale;
    let y = (event.client_y() as f64 - rect.top() - pan_y - offset_y) / scale;
    normalize_point(Point {
        x: x as f32,
        y: y as f32,
    })
}

fn normalize_point(point: Point) -> Option<Point> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return None;
    }
    Some(point)
}

fn sanitize_color(mut color: String) -> String {
    if color.is_empty() {
        return "#1f1f1f".to_string();
    }
    if color.len() > 32 {
        color.truncate(32);
    }
    color
}

fn sanitize_size(size: f32) -> f32 {
    let size = if size.is_finite() { size } else { 6.0 };
    size.max(1.0).min(60.0)
}

fn draw_dot(
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
    let x = point.x as f64 * scale + board_offset_x + pan_x;
    let y = point.y as f64 * scale + board_offset_y + pan_y;
    ctx.set_fill_style_str(color);
    ctx.begin_path();
    let _ = ctx.arc(x, y, size as f64 / 2.0, 0.0, std::f64::consts::PI * 2.0);
    ctx.fill();
}

fn draw_segment(
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
    let from_x = from.x as f64 * scale + board_offset_x + pan_x;
    let from_y = from.y as f64 * scale + board_offset_y + pan_y;
    let to_x = to.x as f64 * scale + board_offset_x + pan_x;
    let to_y = to.y as f64 * scale + board_offset_y + pan_y;

    ctx.set_stroke_style_str(color);
    ctx.set_line_width(size as f64);
    ctx.begin_path();
    ctx.move_to(from_x, from_y);
    ctx.line_to(to_x, to_y);
    ctx.stroke();
}

fn draw_stroke(state: &State, stroke: &Stroke) {
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

fn redraw(state: &mut State) {
    state
        .ctx
        .clear_rect(0.0, 0.0, state.board_width, state.board_height);
    for stroke in &state.strokes {
        draw_stroke(state, stroke);
    }
}

fn start_stroke(state: &mut State, id: String, color: String, size: f32, point: Point) {
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
        state.board_scale,
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

fn move_stroke(state: &mut State, id: &str, point: Point) {
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
                state.board_scale,
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
                state.board_scale,
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

fn end_stroke(state: &mut State, id: &str) {
    state.active_ids.remove(id);
}

fn clear_board(state: &mut State) {
    state.strokes.clear();
    state.active_ids.clear();
    redraw(state);
}

fn remove_stroke(state: &mut State, id: &str) {
    if let Some(index) = state.strokes.iter().position(|stroke| stroke.id == id) {
        state.strokes.remove(index);
        state.active_ids.remove(id);
        redraw(state);
    }
}

fn restore_stroke(state: &mut State, mut stroke: Stroke) {
    stroke.points = stroke
        .points
        .into_iter()
        .filter_map(normalize_point)
        .collect();
    state.strokes.push(stroke);
    redraw(state);
}

fn erase_hits_at_point(state: &mut State, point: Point) -> Vec<String> {
    if state.board_scale <= 0.0 {
        return Vec::new();
    }
    let scale = state.board_scale * state.zoom;
    let px = point.x as f64 * scale + state.board_offset_x + state.pan_x;
    let py = point.y as f64 * scale + state.board_offset_y + state.pan_y;
    let mut removed = Vec::new();
    let mut index = state.strokes.len();

    while index > 0 {
        index -= 1;
        let stroke = &state.strokes[index];
        if state.erase_hits.contains(&stroke.id) {
            continue;
        }
        if stroke_hit(
            stroke,
            px,
            py,
            scale,
            state.board_offset_x,
            state.board_offset_y,
            state.pan_x,
            state.pan_y,
        ) {
            let id = stroke.id.clone();
            state.strokes.remove(index);
            state.active_ids.remove(&id);
            state.erase_hits.insert(id.clone());
            removed.push(id);
        }
    }

    if !removed.is_empty() {
        redraw(state);
    }

    removed
}

fn stroke_hit(
    stroke: &Stroke,
    px: f64,
    py: f64,
    scale: f64,
    offset_x: f64,
    offset_y: f64,
    pan_x: f64,
    pan_y: f64,
) -> bool {
    if stroke.points.is_empty() {
        return false;
    }
    let threshold = (stroke.size as f64 / 2.0).max(6.0);
    if stroke.points.len() == 1 {
        let point = stroke.points[0];
        let dx = point.x as f64 * scale + offset_x + pan_x - px;
        let dy = point.y as f64 * scale + offset_y + pan_y - py;
        return dx * dx + dy * dy <= threshold * threshold;
    }
    for window in stroke.points.windows(2) {
        let start = window[0];
        let end = window[1];
        let distance = distance_to_segment(
            px,
            py,
            start.x as f64 * scale + offset_x + pan_x,
            start.y as f64 * scale + offset_y + pan_y,
            end.x as f64 * scale + offset_x + pan_x,
            end.y as f64 * scale + offset_y + pan_y,
        );
        if distance <= threshold {
            return true;
        }
    }
    false
}

fn distance_to_segment(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
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

fn adopt_strokes(state: &mut State, strokes: Vec<Stroke>) {
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
    redraw(state);
}

fn send_message(socket: &WebSocket, message: &ClientMessage) {
    if socket.ready_state() == WebSocket::OPEN {
        if let Ok(payload) = serde_json::to_string(message) {
            let _ = socket.send_with_str(&payload);
        }
    }
}

fn last_point_for_id(strokes: &[Stroke], id: &str) -> Option<Point> {
    strokes
        .iter()
        .find(|stroke| stroke.id == id)
        .and_then(|stroke| stroke.points.last().copied())
}

fn make_id() -> String {
    let rand = (js_sys::Math::random() * 1_000_000_000.0) as u64;
    let now = js_sys::Date::now() as u64;
    format!("{now:x}{rand:x}")
}
