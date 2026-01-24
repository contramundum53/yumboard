use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CanvasRenderingContext2d, Document, Element, Event, FileReader, HtmlAnchorElement,
    HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement, HtmlSpanElement,
    KeyboardEvent, MessageEvent, PointerEvent, ProgressEvent, WebSocket, Window,
};

use pfboard_shared::{ClientMessage, Point, ServerMessage, Stroke};

#[derive(serde::Serialize, serde::Deserialize)]
struct SaveData {
    version: u8,
    strokes: Vec<Stroke>,
}

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Draw,
    Erase,
    Pan,
    Select,
}

#[derive(Clone, Copy, PartialEq)]
enum SelectionMode {
    None,
    Lasso,
    Move,
    Scale,
    Rotate,
}

enum SelectionHit {
    Move,
    Scale(ScaleHandle),
    Rotate,
    Trash,
}

#[derive(Clone, Copy)]
enum ScaleAxis {
    Both,
    X,
    Y,
}

#[derive(Clone, Copy)]
struct ScaleHandle {
    axis: ScaleAxis,
    anchor: Point,
}

struct Bounds {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

const PALETTE_LIMIT: usize = 8;

struct State {
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    strokes: Vec<Stroke>,
    active_ids: HashSet<String>,
    load_reader: Option<FileReader>,
    load_onload: Option<Closure<dyn FnMut(ProgressEvent)>>,
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
    selected_ids: Vec<String>,
    lasso_points: Vec<Point>,
    palette: Vec<String>,
    selection_mode: SelectionMode,
    transform_center: Point,
    transform_anchor: Point,
    transform_start: Point,
    transform_start_angle: f64,
    transform_snapshot: Vec<Stroke>,
    transform_scale_axis: ScaleAxis,
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
    let palette_el: HtmlElement = get_element(&document, "palette")?;
    let size_input: HtmlInputElement = get_element(&document, "size")?;
    let size_value: HtmlSpanElement = get_element(&document, "sizeValue")?;
    let clear_button: HtmlButtonElement = get_element(&document, "clear")?;
    let save_button: HtmlButtonElement = get_element(&document, "save")?;
    let save_pdf_button: HtmlButtonElement = get_element(&document, "savePdf")?;
    let load_button: HtmlButtonElement = get_element(&document, "load")?;
    let load_file: HtmlInputElement = get_element(&document, "loadFile")?;
    let pen_button: HtmlButtonElement = get_element(&document, "pen")?;
    let lasso_button: HtmlButtonElement = get_element(&document, "lasso")?;
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
        load_reader: None,
        load_onload: None,
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
        selected_ids: Vec::new(),
        lasso_points: Vec::new(),
        palette: Vec::new(),
        selection_mode: SelectionMode::None,
        transform_center: Point { x: 0.0, y: 0.0 },
        transform_anchor: Point { x: 0.0, y: 0.0 },
        transform_start: Point { x: 0.0, y: 0.0 },
        transform_start_angle: 0.0,
        transform_snapshot: Vec::new(),
        transform_scale_axis: ScaleAxis::Both,
    }));

    update_size_label(&size_input, &size_value);
    set_status(&status_el, &status_text, "connecting", "Connecting...");
    set_tool_button(&pen_button, true);
    set_tool_button(&lasso_button, false);
    set_tool_button(&eraser_button, false);
    set_tool_button(&pan_button, false);
    set_canvas_mode(&canvas, Tool::Draw, false);
    {
        let mut state = state.borrow_mut();
        if let Some(color) = normalize_palette_color(&color_input.value()) {
            add_color_to_palette(&mut state.palette, &color);
        }
        render_palette(&document, &palette_el, &state.palette, &color_input.value());
    }

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
                ServerMessage::StrokeReplace { stroke } => {
                    replace_stroke_local(&mut state, stroke);
                    redraw(&mut state);
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
        let key_state = state.clone();
        let onkeydown = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            let key = event.key();
            let modifier = event.meta_key() || event.ctrl_key();
            if !modifier {
                if key == "Delete" || key == "Backspace" {
                    let ids = {
                        let mut state = key_state.borrow_mut();
                        if state.selected_ids.is_empty() {
                            return;
                        }
                        let ids = state.selected_ids.clone();
                        for id in &ids {
                            remove_stroke(&mut state, id);
                        }
                        state.selected_ids.clear();
                        state.selection_mode = SelectionMode::None;
                        ids
                    };
                    send_message(&key_socket, &ClientMessage::Remove { ids });
                    event.prevent_default();
                }
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
        let lasso_button_cb = lasso_button.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            state.tool = Tool::Erase;
            state.drawing = false;
            state.current_id = None;
            state.erasing = false;
            state.panning = false;
            state.erase_hits.clear();
            state.lasso_points.clear();
            state.selected_ids.clear();
            state.selection_mode = SelectionMode::None;
            set_tool_button(&pan_button_cb, state.tool == Tool::Pan);
            set_tool_button(&eraser_button_cb, state.tool == Tool::Erase);
            set_tool_button(&pen_button_cb, state.tool == Tool::Draw);
            set_tool_button(&lasso_button_cb, state.tool == Tool::Select);
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
        let lasso_button_cb = lasso_button.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            state.tool = Tool::Draw;
            state.drawing = false;
            state.current_id = None;
            state.erasing = false;
            state.panning = false;
            state.erase_hits.clear();
            state.lasso_points.clear();
            state.selected_ids.clear();
            state.selection_mode = SelectionMode::None;
            set_tool_button(&eraser_button_cb, state.tool == Tool::Erase);
            set_tool_button(&pan_button_cb, state.tool == Tool::Pan);
            set_tool_button(&pen_button_cb, state.tool == Tool::Draw);
            set_tool_button(&lasso_button_cb, state.tool == Tool::Select);
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
        let lasso_button_cb = lasso_button.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            state.tool = Tool::Select;
            state.drawing = false;
            state.current_id = None;
            state.erasing = false;
            state.panning = false;
            state.erase_hits.clear();
            state.lasso_points.clear();
            state.selected_ids.clear();
            state.selection_mode = SelectionMode::None;
            set_tool_button(&eraser_button_cb, state.tool == Tool::Erase);
            set_tool_button(&pan_button_cb, state.tool == Tool::Pan);
            set_tool_button(&pen_button_cb, state.tool == Tool::Draw);
            set_tool_button(&lasso_button_cb, state.tool == Tool::Select);
            set_canvas_mode(&state.canvas, state.tool, false);
        });
        lasso_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let tool_state = state.clone();
        let eraser_button_cb = eraser_button.clone();
        let pan_button_cb = pan_button.clone();
        let pen_button_cb = pen_button.clone();
        let lasso_button_cb = lasso_button.clone();
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
            state.lasso_points.clear();
            state.selected_ids.clear();
            state.selection_mode = SelectionMode::None;
            set_tool_button(&eraser_button_cb, state.tool == Tool::Erase);
            set_tool_button(&pan_button_cb, state.tool == Tool::Pan);
            set_tool_button(&pen_button_cb, state.tool == Tool::Draw);
            set_tool_button(&lasso_button_cb, state.tool == Tool::Select);
            set_canvas_mode(&state.canvas, state.tool, false);
        });
        pan_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let palette_state = state.clone();
        let palette_el_cb = palette_el.clone();
        let palette_el_listener = palette_el.clone();
        let color_input = color_input.clone();
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let color = match palette_color_from_event(&event) {
                Some(color) => color,
                None => return,
            };
            color_input.set_value(&color);
            let mut state = palette_state.borrow_mut();
            add_color_to_palette(&mut state.palette, &color);
            render_palette(&document, &palette_el_cb, &state.palette, &color_input.value());
        });
        palette_el_listener.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
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
            add_color_to_palette(&mut state.palette, &color);
            render_palette(&document, &palette_el_cb, &state.palette, &color_input_cb.value());
        });
        color_input_listener.add_event_listener_with_callback("input", oninput.as_ref().unchecked_ref())?;
        oninput.forget();
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
        let save_state = state.clone();
        let document = document.clone();
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
                    anchor.set_download("pfboard.json");
                    anchor.click();
                }
            }
        });
        save_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let save_state = state.clone();
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let html = build_pdf_html(&save_state.borrow(), false);
            open_print_window(&document, &html);
        });
        save_pdf_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let load_file = load_file.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            load_file.set_value("");
            load_file.click();
        });
        load_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let load_state_onload = state.clone();
        let load_socket_onload = socket.clone();
        let onload = Closure::<dyn FnMut(ProgressEvent)>::new(move |event: ProgressEvent| {
            let target = match event.target() {
                Some(target) => target,
                None => return,
            };
            let reader: FileReader = match target.dyn_into() {
                Ok(reader) => reader,
                Err(_) => return,
            };
            let Some(result) = reader.result().ok() else {
                return;
            };
            let Some(text) = result.as_string() else {
                return;
            };
            let Some(strokes) = parse_load_payload(&text) else {
                return;
            };
            {
                let mut state = load_state_onload.borrow_mut();
                adopt_strokes(&mut state, strokes.clone());
                state.load_reader = None;
            }
            send_message(&load_socket_onload, &ClientMessage::Load { strokes });
        });
        state.borrow_mut().load_onload = Some(onload);

        let load_file_cb = load_file.clone();
        let load_state_onchange = state.clone();
        let onchange = Closure::<dyn FnMut(Event)>::new(move |_| {
            let files = load_file_cb.files();
            let file = files.and_then(|list| list.get(0));
            let Some(file) = file else {
                return;
            };
            let reader = match FileReader::new() {
                Ok(reader) => reader,
                Err(_) => return,
            };
            if let Some(handler) = load_state_onchange.borrow().load_onload.as_ref() {
                reader.set_onload(Some(handler.as_ref().unchecked_ref()));
            }
            let _ = reader.read_as_text(&file);
            load_state_onchange
                .borrow_mut()
                .load_reader
                .replace(reader);
        });
        load_file.add_event_listener_with_callback("change", onchange.as_ref().unchecked_ref())?;
        onchange.forget();
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
            if tool == Tool::Select {
                let rect = down_canvas.get_bounding_client_rect();
                let screen_x = event.client_x() as f64 - rect.left();
                let screen_y = event.client_y() as f64 - rect.top();
                let (pan_x, pan_y, zoom, board_scale, offset_x, offset_y) = {
                    let state = down_state.borrow();
                    (
                        state.pan_x,
                        state.pan_y,
                        state.zoom,
                        state.board_scale,
                        state.board_offset_x,
                        state.board_offset_y,
                    )
                };
                let world_point = match event_to_point(
                    &down_canvas,
                    &event,
                    pan_x,
                    pan_y,
                    zoom,
                    board_scale,
                    offset_x,
                    offset_y,
                ) {
                    Some(point) => point,
                    None => return,
                };
                let mut state = down_state.borrow_mut();
                if let Some(hit) = selection_hit_test(&state, screen_x, screen_y) {
                    match hit {
                        SelectionHit::Trash => {
                            let ids = state.selected_ids.clone();
                            for id in &ids {
                                remove_stroke(&mut state, id);
                            }
                            state.selected_ids.clear();
                            state.selection_mode = SelectionMode::None;
                            state.transform_snapshot.clear();
                            state.lasso_points.clear();
                            redraw(&mut state);
                            send_message(&down_socket, &ClientMessage::Remove { ids });
                            return;
                        }
                        SelectionHit::Rotate => {
                            if let Some(center) = selection_center(&state) {
                                state.selection_mode = SelectionMode::Rotate;
                                state.transform_center = center;
                                state.transform_start_angle =
                                    angle_between(center, world_point);
                                state.transform_snapshot = selected_strokes(&state);
                            }
                        }
                        SelectionHit::Scale(handle) => {
                            let dx = (world_point.x - handle.anchor.x) as f64;
                            let dy = (world_point.y - handle.anchor.y) as f64;
                            if dx.abs() > f64::EPSILON || dy.abs() > f64::EPSILON {
                                state.selection_mode = SelectionMode::Scale;
                                state.transform_anchor = handle.anchor;
                                state.transform_start = world_point;
                                state.transform_scale_axis = handle.axis;
                                state.transform_snapshot = selected_strokes(&state);
                            }
                        }
                        SelectionHit::Move => {
                            state.selection_mode = SelectionMode::Move;
                            state.transform_start = world_point;
                            state.transform_snapshot = selected_strokes(&state);
                        }
                    }
                    let _ = down_canvas.set_pointer_capture(event.pointer_id());
                    return;
                }
                state.selected_ids.clear();
                state.lasso_points.clear();
                state.lasso_points.push(world_point);
                state.selection_mode = SelectionMode::Lasso;
                redraw(&mut state);
                let _ = down_canvas.set_pointer_capture(event.pointer_id());
                return;
            }
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
            let (pan_x, pan_y, zoom, board_scale, offset_x, offset_y) = {
                let state = down_state.borrow();
                (
                    state.pan_x,
                    state.pan_y,
                    state.zoom,
                    state.board_scale,
                    state.board_offset_x,
                    state.board_offset_y,
                )
            };
            let point = match event_to_point(
                &down_canvas,
                &event,
                pan_x,
                pan_y,
                zoom,
                board_scale,
                offset_x,
                offset_y,
            ) {
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
            if tool == Tool::Select {
                let rect = move_canvas.get_bounding_client_rect();
                let screen_x = event.client_x() as f64 - rect.left();
                let screen_y = event.client_y() as f64 - rect.top();
                let (pan_x, pan_y, zoom, board_scale, offset_x, offset_y) = {
                    let state = move_state.borrow();
                    (
                        state.pan_x,
                        state.pan_y,
                        state.zoom,
                        state.board_scale,
                        state.board_offset_x,
                        state.board_offset_y,
                    )
                };
                let world_point = match event_to_point(
                    &move_canvas,
                    &event,
                    pan_x,
                    pan_y,
                    zoom,
                    board_scale,
                    offset_x,
                    offset_y,
                ) {
                    Some(point) => point,
                    None => return,
                };
                let mut state = move_state.borrow_mut();
                match state.selection_mode {
                    SelectionMode::Lasso => {
                        state.lasso_points.push(world_point);
                        redraw(&mut state);
                    }
                    SelectionMode::Move => {
                        let delta_x = world_point.x - state.transform_start.x;
                        let delta_y = world_point.y - state.transform_start.y;
                        let updated =
                            apply_translation(&state.transform_snapshot, delta_x, delta_y);
                        apply_transformed_strokes(&mut state, &updated);
                        for stroke in updated {
                            send_message(
                                &move_socket,
                                &ClientMessage::StrokeReplace { stroke },
                            );
                        }
                    }
                    SelectionMode::Scale => {
                        let anchor = state.transform_anchor;
                        let start = state.transform_start;
                        let axis = state.transform_scale_axis;
                        let dx0 = (start.x - anchor.x) as f64;
                        let dy0 = (start.y - anchor.y) as f64;
                        let dx1 = (world_point.x - anchor.x) as f64;
                        let dy1 = (world_point.y - anchor.y) as f64;
                        let mut sx = if dx0.abs() > f64::EPSILON {
                            dx1 / dx0
                        } else {
                            1.0
                        };
                        let mut sy = if dy0.abs() > f64::EPSILON {
                            dy1 / dy0
                        } else {
                            1.0
                        };
                        match axis {
                            ScaleAxis::Both => {}
                            ScaleAxis::X => sy = 1.0,
                            ScaleAxis::Y => sx = 1.0,
                        }
                        sx = clamp_scale(sx, 0.05);
                        sy = clamp_scale(sy, 0.05);
                        let updated = apply_scale_xy(
                            &state.transform_snapshot,
                            anchor,
                            sx,
                            sy,
                        );
                        apply_transformed_strokes(&mut state, &updated);
                        for stroke in updated {
                            send_message(&move_socket, &ClientMessage::StrokeReplace { stroke });
                        }
                    }
                    SelectionMode::Rotate => {
                        let angle = angle_between(state.transform_center, world_point);
                        let delta = angle - state.transform_start_angle;
                        let updated = apply_rotation(
                            &state.transform_snapshot,
                            state.transform_center,
                            delta,
                        );
                        apply_transformed_strokes(&mut state, &updated);
                        for stroke in updated {
                            send_message(
                                &move_socket,
                                &ClientMessage::StrokeReplace { stroke },
                            );
                        }
                    }
                    SelectionMode::None => {
                        if selection_hit_test(&state, screen_x, screen_y).is_some() {
                            set_canvas_mode(&state.canvas, Tool::Select, false);
                        }
                    }
                }
                return;
            }
            if tool == Tool::Erase {
                let (pan_x, pan_y, zoom, board_scale, offset_x, offset_y) = {
                    let state = move_state.borrow();
                    (
                        state.pan_x,
                        state.pan_y,
                        state.zoom,
                        state.board_scale,
                        state.board_offset_x,
                        state.board_offset_y,
                    )
                };
                let point = match event_to_point(
                    &move_canvas,
                    &event,
                    pan_x,
                    pan_y,
                    zoom,
                    board_scale,
                    offset_x,
                    offset_y,
                ) {
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
            let (pan_x, pan_y, zoom, board_scale, offset_x, offset_y) = {
                let state = move_state.borrow();
                (
                    state.pan_x,
                    state.pan_y,
                    state.zoom,
                    state.board_scale,
                    state.board_offset_x,
                    state.board_offset_y,
                )
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
                let point = match event_to_point(
                    &move_canvas,
                    &event,
                    pan_x,
                    pan_y,
                    zoom,
                    board_scale,
                    offset_x,
                    offset_y,
                ) {
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
            if stop_state.borrow().tool == Tool::Select {
                event.prevent_default();
                if stop_canvas.has_pointer_capture(event.pointer_id()) {
                    let _ = stop_canvas.release_pointer_capture(event.pointer_id());
                }
                let mut state = stop_state.borrow_mut();
                match state.selection_mode {
                    SelectionMode::Lasso => {
                        finalize_lasso_selection(&mut state);
                    }
                    SelectionMode::Move
                    | SelectionMode::Scale
                    | SelectionMode::Rotate
                    | SelectionMode::None => {}
                }
                state.selection_mode = SelectionMode::None;
                state.transform_snapshot.clear();
                state.lasso_points.clear();
                redraw(&mut state);
                return;
            }
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
    let session_id = session_id_from_location(&location);
    if let Some(session_id) = session_id {
        Ok(format!("{scheme}://{host}/ws/{session_id}"))
    } else {
        Ok(format!("{scheme}://{host}/ws"))
    }
}

fn session_id_from_location(location: &web_sys::Location) -> Option<String> {
    let path = location.pathname().ok()?;
    let mut parts = path.trim_matches('/').split('/');
    if parts.next()? != "s" {
        return None;
    }
    let session_id = parts.next()?;
    if session_id.is_empty() {
        None
    } else {
        Some(session_id.to_string())
    }
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
        Tool::Select => "default",
    };
    if let Ok(element) = canvas.clone().dyn_into::<HtmlElement>() {
        let _ = element.style().set_property("cursor", cursor);
    }
}

fn set_status(status_el: &Element, status_text: &Element, state: &str, text: &str) {
    let _ = status_el.set_attribute("data-state", state);
    status_text.set_text_content(Some(text));
}

fn parse_load_payload(text: &str) -> Option<Vec<Stroke>> {
    if let Some(strokes) = try_parse_strokes(text) {
        return Some(strokes);
    }
    let trimmed = text.trim();
    if let Some(payload) = extract_data_url_payload(trimmed) {
        if let Some(strokes) = try_parse_strokes(&payload) {
            return Some(strokes);
        }
        if let Some(decoded) = decode_uri_string(&payload) {
            if let Some(strokes) = try_parse_strokes(&decoded) {
                return Some(strokes);
            }
        }
    }
    if let Some(decoded) = decode_uri_string(trimmed) {
        if let Some(strokes) = try_parse_strokes(&decoded) {
            return Some(strokes);
        }
    }
    None
}

fn try_parse_strokes(text: &str) -> Option<Vec<Stroke>> {
    if let Ok(data) = serde_json::from_str::<SaveData>(text) {
        return Some(data.strokes);
    }
    serde_json::from_str::<Vec<Stroke>>(text).ok()
}

fn extract_data_url_payload(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("data:") {
        return None;
    }
    let (_, payload) = trimmed.split_once(',')?;
    Some(payload.to_string())
}

fn decode_uri_string(text: &str) -> Option<String> {
    js_sys::decode_uri_component(text)
        .ok()
        .and_then(|value| value.as_string())
}

fn normalize_palette_color(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_lowercase())
    }
}

fn add_color_to_palette(palette: &mut Vec<String>, color: &str) {
    let Some(color) = normalize_palette_color(color) else {
        return;
    };
    palette.retain(|item| item != &color);
    palette.insert(0, color);
    if palette.len() > PALETTE_LIMIT {
        palette.truncate(PALETTE_LIMIT);
    }
}

fn render_palette(document: &Document, palette_el: &HtmlElement, colors: &[String], current: &str) {
    palette_el.set_inner_html("");
    let current = normalize_palette_color(current);
    for color in colors {
        let Ok(element) = document.create_element("button") else {
            continue;
        };
        let Ok(button) = element.dyn_into::<HtmlButtonElement>() else {
            continue;
        };
        let _ = button.set_attribute("type", "button");
        let _ = button.set_attribute("data-color", color);
        let _ = button.set_attribute("aria-label", &format!("Use color {color}"));
        let class_name = if current.as_deref() == Some(color.as_str()) {
            "swatch active"
        } else {
            "swatch"
        };
        let _ = button.set_attribute("class", class_name);
        let _ = button.style().set_property("background", color);
        let _ = palette_el.append_child(&button);
    }
}

fn palette_color_from_event(event: &Event) -> Option<String> {
    let mut current = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok());
    while let Some(element) = current {
        if let Some(color) = element.get_attribute("data-color") {
            return normalize_palette_color(&color);
        }
        current = element.parent_element().map(|parent| parent.into());
    }
    None
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
    board_scale: f64,
    board_offset_x: f64,
    board_offset_y: f64,
) -> Option<Point> {
    let rect = canvas.get_bounding_client_rect();
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    if board_scale <= 0.0 {
        return None;
    }
    let scale = board_scale * zoom;
    let x = (event.client_x() as f64 - rect.left() - pan_x - board_offset_x) / scale;
    let y = (event.client_y() as f64 - rect.top() - pan_y - board_offset_y) / scale;
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
    draw_selection_overlay(state);
}

fn draw_selection_overlay(state: &mut State) {
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
        let (left, top) = world_to_screen(state, Point { x: bounds.min_x as f32, y: bounds.min_y as f32 });
        let (right, bottom) = world_to_screen(state, Point { x: bounds.max_x as f32, y: bounds.max_y as f32 });
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

fn world_to_screen(state: &State, point: Point) -> (f64, f64) {
    let scale = state.board_scale * state.zoom;
    let x = point.x as f64 * scale + state.board_offset_x + state.pan_x;
    let y = point.y as f64 * scale + state.board_offset_y + state.pan_y;
    (x, y)
}

fn selection_bounds(state: &State) -> Option<Bounds> {
    if state.selected_ids.is_empty() {
        return None;
    }
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for stroke in &state.strokes {
        if !state.selected_ids.iter().any(|id| id == &stroke.id) {
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

fn selection_center(state: &State) -> Option<Point> {
    let bounds = selection_bounds(state)?;
    Some(Point {
        x: ((bounds.min_x + bounds.max_x) / 2.0) as f32,
        y: ((bounds.min_y + bounds.max_y) / 2.0) as f32,
    })
}

fn selection_hit_test(state: &State, screen_x: f64, screen_y: f64) -> Option<SelectionHit> {
    let bounds = selection_bounds(state)?;
    let (left, top) = world_to_screen(state, Point { x: bounds.min_x as f32, y: bounds.min_y as f32 });
    let (right, bottom) = world_to_screen(state, Point { x: bounds.max_x as f32, y: bounds.max_y as f32 });
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

fn angle_between(center: Point, point: Point) -> f64 {
    let dx = point.x as f64 - center.x as f64;
    let dy = point.y as f64 - center.y as f64;
    dy.atan2(dx)
}

fn selected_strokes(state: &State) -> Vec<Stroke> {
    state
        .strokes
        .iter()
        .filter(|stroke| state.selected_ids.iter().any(|id| id == &stroke.id))
        .cloned()
        .collect()
}

fn apply_transformed_strokes(state: &mut State, strokes: &[Stroke]) {
    for stroke in strokes {
        replace_stroke_local(state, stroke.clone());
    }
    redraw(state);
}

fn apply_translation(strokes: &[Stroke], dx: f32, dy: f32) -> Vec<Stroke> {
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

fn apply_scale_xy(strokes: &[Stroke], center: Point, sx: f64, sy: f64) -> Vec<Stroke> {
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

fn clamp_scale(value: f64, min_abs: f64) -> f64 {
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

fn apply_rotation(strokes: &[Stroke], center: Point, angle: f64) -> Vec<Stroke> {
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

fn finalize_lasso_selection(state: &mut State) {
    if state.lasso_points.len() < 3 {
        state.lasso_points.clear();
        return;
    }
    let polygon = state.lasso_points.clone();
    let mut selected = Vec::new();
    for stroke in &state.strokes {
        if stroke
            .points
            .iter()
            .any(|point| point_in_polygon(*point, &polygon))
        {
            selected.push(stroke.id.clone());
        }
    }
    state.selected_ids = selected;
}

fn point_in_polygon(point: Point, polygon: &[Point]) -> bool {
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

fn build_pdf_html(state: &State, include_background: bool) -> String {
    let size = if state.board_scale > 0.0 {
        state.board_scale
    } else {
        1.0
    };
    let mut paths = String::new();
    for stroke in &state.strokes {
        if stroke.points.is_empty() {
            continue;
        }
        let mut data = String::new();
        for (index, point) in stroke.points.iter().enumerate() {
            let x = point.x as f64 * size;
            let y = point.y as f64 * size;
            if index == 0 {
                data.push_str(&format!("M {} {}", x, y));
            } else {
                data.push_str(&format!(" L {} {}", x, y));
            }
        }
        let color = stroke.color.clone();
        let width = stroke.size;
        paths.push_str(&format!(
            "<path d=\"{}\" stroke=\"{}\" stroke-width=\"{}\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\" />",
            data, color, width
        ));
        if stroke.points.len() == 1 {
            let p = stroke.points[0];
            let cx = p.x as f64 * size;
            let cy = p.y as f64 * size;
            let r = stroke.size as f64 / 2.0;
            paths.push_str(&format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"{}\" />",
                cx, cy, r, color
            ));
        }
    }

    let background = if include_background {
        "<rect width=\"100%\" height=\"100%\" fill=\"#ffffff\" />"
    } else {
        ""
    };

    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\" /><style>@page{{margin:0;}}body{{margin:0;}}svg{{width:100vw;height:100vh;}}</style></head><body><svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {size}\">{background}{paths}</svg><script>window.onload=()=>{{window.print();}}</script></body></html>",
        size = size,
        background = background,
        paths = paths
    )
}

fn open_print_window(document: &Document, html: &str) {
    let window = match document.default_view() {
        Some(window) => window,
        None => return,
    };
    let encoded = js_sys::encode_uri_component(html);
    let url = format!("data:text/html;charset=utf-8,{encoded}");
    let _ = window.open_with_url_and_target(&url, "_blank");
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
    state.selected_ids.clear();
    state.lasso_points.clear();
    state.selection_mode = SelectionMode::None;
    redraw(state);
}

fn remove_stroke(state: &mut State, id: &str) {
    if let Some(index) = state.strokes.iter().position(|stroke| stroke.id == id) {
        state.strokes.remove(index);
        state.active_ids.remove(id);
        state.selected_ids.retain(|selected| selected != id);
        redraw(state);
    }
}

fn replace_stroke_local(state: &mut State, stroke: Stroke) {
    if let Some(index) = state.strokes.iter().position(|item| item.id == stroke.id) {
        state.strokes[index] = stroke;
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
    state.selected_ids.clear();
    state.lasso_points.clear();
    state.selection_mode = SelectionMode::None;
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
