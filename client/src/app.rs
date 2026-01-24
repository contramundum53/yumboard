use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CanvasRenderingContext2d, Event, FileReader, HtmlAnchorElement, HtmlButtonElement,
    HtmlCanvasElement, HtmlElement, HtmlInputElement, HtmlSpanElement, KeyboardEvent, MessageEvent,
    PointerEvent, ProgressEvent, WebSocket,
};

use pfboard_shared::{ClientMessage, ServerMessage};

use crate::actions::{
    adopt_strokes, apply_transformed_strokes, clear_board, end_stroke, erase_hits_at_point,
    finalize_lasso_selection, last_point_for_id, move_stroke, remove_stroke, replace_stroke_local,
    restore_stroke, sanitize_size, start_stroke,
};
use crate::dom::{
    event_to_point, get_element, resize_canvas, set_canvas_mode, set_status, set_tool_button,
    update_size_label,
};
use crate::geometry::{
    angle_between, apply_rotation, apply_scale_xy, apply_translation, clamp_scale,
    selected_strokes, selection_center, selection_hit_test,
};
use crate::net::{send_message, websocket_url};
use crate::palette::{palette_action_from_event, render_palette, PaletteAction};
use crate::persistence::{build_pdf_html, open_print_window, parse_load_payload, SaveData};
use crate::render::redraw;
use crate::state::{
    DrawMode, DrawState, EraseMode, Mode, PanMode, ScaleAxis, SelectMode, SelectState,
    SelectionHit, State, DEFAULT_PALETTE,
};
use crate::util::make_id;

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
    let save_menu: HtmlElement = get_element(&document, "saveMenu")?;
    let save_json_button: HtmlButtonElement = get_element(&document, "saveJson")?;
    let save_pdf_button: HtmlButtonElement = get_element(&document, "savePdf")?;
    let load_button: HtmlButtonElement = get_element(&document, "load")?;
    let load_file: HtmlInputElement = get_element(&document, "loadFile")?;
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
        board_offset_x: 0.0,
        board_offset_y: 0.0,
        zoom: 1.0,
        pan_x: 0.0,
        pan_y: 0.0,
        palette: DEFAULT_PALETTE
            .iter()
            .map(|value| value.to_string())
            .collect(),
        palette_last_selected: 0,
        mode: Mode::Draw(DrawState {
            mode: DrawMode::Idle,
            palette_selected: Some(0),
            palette_add_mode: false,
        }),
    }));

    update_size_label(&size_input, &size_value);
    set_status(&status_el, &status_text, "connecting", "Connecting...");
    set_tool_button(&lasso_button, false);
    set_tool_button(&eraser_button, false);
    set_tool_button(&pan_button, false);
    set_canvas_mode(&canvas, &state.borrow().mode, false);
    {
        let state = state.borrow();
        if let Some(index) = state.palette_selected() {
            if let Some(color) = state.palette.get(index).cloned() {
                color_input.set_value(&color);
            }
        }
        render_palette(
            &document,
            &palette_el,
            &state.palette,
            state.palette_selected(),
        );
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
                    redraw(&mut state);
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
                        redraw(&mut state);
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
        let lasso_button_cb = lasso_button.clone();
        let palette_el_cb = palette_el.clone();
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            state.mode = Mode::Erase(EraseMode::Idle);
            let is_pan = matches!(state.mode, Mode::Pan(_));
            let is_erase = matches!(state.mode, Mode::Erase(_));
            let is_select = matches!(state.mode, Mode::Select(_));
            set_tool_button(&pan_button_cb, is_pan);
            set_tool_button(&eraser_button_cb, is_erase);
            set_tool_button(&lasso_button_cb, is_select);
            set_canvas_mode(&state.canvas, &state.mode, false);
            render_palette(
                &document,
                &palette_el_cb,
                &state.palette,
                state.palette_selected(),
            );
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
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            state.mode = Mode::Select(SelectState {
                selected_ids: Vec::new(),
                mode: SelectMode::Idle,
            });
            let is_pan = matches!(state.mode, Mode::Pan(_));
            let is_erase = matches!(state.mode, Mode::Erase(_));
            let is_select = matches!(state.mode, Mode::Select(_));
            set_tool_button(&eraser_button_cb, is_erase);
            set_tool_button(&pan_button_cb, is_pan);
            set_tool_button(&lasso_button_cb, is_select);
            set_canvas_mode(&state.canvas, &state.mode, false);
            render_palette(
                &document,
                &palette_el_cb,
                &state.palette,
                state.palette_selected(),
            );
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
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            let going_to_pan = !matches!(state.mode, Mode::Pan(_));
            if !going_to_pan {
                if state.palette_last_selected >= state.palette.len() {
                    state.palette_last_selected = 0;
                }
                let palette_selected = if state.palette.is_empty() {
                    None
                } else {
                    if let Some(color) = state.palette.get(state.palette_last_selected).cloned() {
                        color_input_cb.set_value(&color);
                    }
                    Some(state.palette_last_selected)
                };
                state.mode = Mode::Draw(DrawState {
                    mode: DrawMode::Idle,
                    palette_selected,
                    palette_add_mode: false,
                });
            } else {
                state.mode = Mode::Pan(PanMode::Idle);
            }
            let is_pan = matches!(state.mode, Mode::Pan(_));
            let is_erase = matches!(state.mode, Mode::Erase(_));
            let is_select = matches!(state.mode, Mode::Select(_));
            set_tool_button(&eraser_button_cb, is_erase);
            set_tool_button(&pan_button_cb, is_pan);
            set_tool_button(&lasso_button_cb, is_select);
            set_canvas_mode(&state.canvas, &state.mode, false);
            render_palette(
                &document,
                &palette_el_cb,
                &state.palette,
                state.palette_selected(),
            );
        });
        pan_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
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
        let document = document.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let action = match palette_action_from_event(&event) {
                Some(action) => action,
                None => return,
            };
            let mut state = palette_state.borrow_mut();
            if !matches!(state.mode, Mode::Draw(_)) {
                state.mode = Mode::Draw(DrawState {
                    mode: DrawMode::Idle,
                    palette_selected: state.palette_selected(),
                    palette_add_mode: false,
                });
            }
            set_tool_button(&eraser_button_cb, false);
            set_tool_button(&pan_button_cb, false);
            set_tool_button(&lasso_button_cb, false);
            set_canvas_mode(&state.canvas, &state.mode, false);
            match action {
                PaletteAction::Add => {
                    state.mode = Mode::Draw(DrawState {
                        mode: DrawMode::Idle,
                        palette_selected: state.palette_selected(),
                        palette_add_mode: true,
                    });
                    render_palette(
                        &document,
                        &palette_el_cb,
                        &state.palette,
                        state.palette_selected(),
                    );
                    color_input.click();
                }
                PaletteAction::Select(index) => {
                    if index >= state.palette.len() {
                        return;
                    }
                    let already_selected = state.palette_selected() == Some(index);
                    if already_selected && matches!(state.mode, Mode::Draw(_)) {
                        if let Mode::Draw(draw) = &mut state.mode {
                            draw.palette_add_mode = false;
                        }
                        color_input.click();
                        return;
                    }
                    state.palette_last_selected = index;
                    state.mode = Mode::Draw(DrawState {
                        mode: DrawMode::Idle,
                        palette_selected: Some(index),
                        palette_add_mode: false,
                    });
                    if let Some(color) = state.palette.get(index).cloned() {
                        color_input.set_value(&color);
                    }
                    render_palette(
                        &document,
                        &palette_el_cb,
                        &state.palette,
                        state.palette_selected(),
                    );
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
            let add_mode = match &state.mode {
                Mode::Draw(draw) => draw.palette_add_mode,
                _ => return,
            };
            if add_mode {
                state.palette.push(color);
                let index = state.palette.len().saturating_sub(1);
                state.palette_last_selected = index;
                if let Mode::Draw(draw) = &mut state.mode {
                    draw.palette_selected = Some(index);
                    draw.palette_add_mode = false;
                }
            } else {
                let mut selected = match &state.mode {
                    Mode::Draw(draw) => draw.palette_selected,
                    _ => None,
                }
                .unwrap_or(state.palette_last_selected);
                if state.palette.is_empty() {
                    if let Mode::Draw(draw) = &mut state.mode {
                        draw.palette_selected = None;
                    }
                } else {
                    if selected >= state.palette.len() {
                        selected = 0;
                    }
                    if let Some(entry) = state.palette.get_mut(selected) {
                        *entry = color;
                    }
                    state.palette_last_selected = selected;
                    if let Mode::Draw(draw) = &mut state.mode {
                        draw.palette_selected = Some(selected);
                        draw.palette_add_mode = false;
                    }
                }
            }
            render_palette(
                &document,
                &palette_el_cb,
                &state.palette,
                state.palette_selected(),
            );
        });
        color_input_listener
            .add_event_listener_with_callback("input", oninput.as_ref().unchecked_ref())?;
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
                    anchor.set_download("pfboard.json");
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
            load_state_onchange.borrow_mut().load_reader.replace(reader);
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
            let (is_select, is_pan, is_erase) = {
                let state = down_state.borrow();
                (
                    matches!(state.mode, Mode::Select(_)),
                    matches!(state.mode, Mode::Pan(_)),
                    matches!(state.mode, Mode::Erase(_)),
                )
            };
            if is_select {
                let rect = down_canvas.get_bounding_client_rect();
                let screen_x = event.client_x() as f64 - rect.left();
                let screen_y = event.client_y() as f64 - rect.top();
                let (pan_x, pan_y, zoom, offset_x, offset_y) = {
                    let state = down_state.borrow();
                    (
                        state.pan_x,
                        state.pan_y,
                        state.zoom,
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
                    offset_x,
                    offset_y,
                ) {
                    Some(point) => point,
                    None => return,
                };
                let hit = {
                    let state = down_state.borrow();
                    match &state.mode {
                        Mode::Select(select) => {
                            selection_hit_test(&state, select, screen_x, screen_y)
                        }
                        _ => None,
                    }
                };
                let selection_ids = {
                    let state = down_state.borrow();
                    match &state.mode {
                        Mode::Select(select) => select.selected_ids.clone(),
                        _ => Vec::new(),
                    }
                };
                let snapshot = {
                    let state = down_state.borrow();
                    match &state.mode {
                        Mode::Select(select) => selected_strokes(&state.strokes, select),
                        _ => Vec::new(),
                    }
                };
                let center = {
                    let state = down_state.borrow();
                    match &state.mode {
                        Mode::Select(select) => selection_center(&state.strokes, select),
                        _ => None,
                    }
                };
                let mut state = down_state.borrow_mut();
                if !matches!(&state.mode, Mode::Select(_)) {
                    state.mode = Mode::Select(SelectState {
                        selected_ids: Vec::new(),
                        mode: SelectMode::Idle,
                    });
                }
                if let Some(hit) = hit {
                    match hit {
                        SelectionHit::Trash => {
                            let ids = selection_ids;
                            for id in &ids {
                                remove_stroke(&mut state, id);
                            }
                            if let Mode::Select(select) = &mut state.mode {
                                select.selected_ids.clear();
                                select.mode = SelectMode::Idle;
                            }
                            redraw(&mut state);
                            send_message(&down_socket, &ClientMessage::Remove { ids });
                            return;
                        }
                        SelectionHit::Rotate => {
                            if let Some(center) = center {
                                let snapshot = snapshot.clone();
                                if let Mode::Select(select) = &mut state.mode {
                                    select.mode = SelectMode::Rotate {
                                        center,
                                        start_angle: angle_between(center, world_point),
                                        snapshot,
                                    };
                                    let ids = selection_ids.clone();
                                    if !ids.is_empty() {
                                        send_message(
                                            &down_socket,
                                            &ClientMessage::TransformStart { ids },
                                        );
                                    }
                                }
                            }
                        }
                        SelectionHit::Scale(handle) => {
                            let dx = (world_point.x - handle.anchor.x) as f64;
                            let dy = (world_point.y - handle.anchor.y) as f64;
                            if dx.abs() > f64::EPSILON || dy.abs() > f64::EPSILON {
                                let snapshot = snapshot.clone();
                                if let Mode::Select(select) = &mut state.mode {
                                    select.mode = SelectMode::Scale {
                                        anchor: handle.anchor,
                                        start: world_point,
                                        axis: handle.axis,
                                        snapshot,
                                    };
                                    let ids = selection_ids.clone();
                                    if !ids.is_empty() {
                                        send_message(
                                            &down_socket,
                                            &ClientMessage::TransformStart { ids },
                                        );
                                    }
                                }
                            }
                        }
                        SelectionHit::Move => {
                            let snapshot = snapshot.clone();
                            if let Mode::Select(select) = &mut state.mode {
                                select.mode = SelectMode::Move {
                                    start: world_point,
                                    snapshot,
                                };
                                let ids = selection_ids.clone();
                                if !ids.is_empty() {
                                    send_message(&down_socket, &ClientMessage::TransformStart { ids });
                                }
                            }
                        }
                    }
                    let _ = down_canvas.set_pointer_capture(event.pointer_id());
                    return;
                }
                if let Mode::Select(select) = &mut state.mode {
                    select.selected_ids.clear();
                    select.mode = SelectMode::Lasso {
                        points: vec![world_point],
                    };
                }
                redraw(&mut state);
                let _ = down_canvas.set_pointer_capture(event.pointer_id());
                return;
            }
            if is_pan {
                let mut state = down_state.borrow_mut();
                state.mode = Mode::Pan(PanMode::Active {
                    start_x: event.client_x() as f64,
                    start_y: event.client_y() as f64,
                    origin_x: state.pan_x,
                    origin_y: state.pan_y,
                });
                set_canvas_mode(&state.canvas, &state.mode, true);
                let _ = down_canvas.set_pointer_capture(event.pointer_id());
                return;
            }
            let (pan_x, pan_y, zoom, offset_x, offset_y) = {
                let state = down_state.borrow();
                (
                    state.pan_x,
                    state.pan_y,
                    state.zoom,
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
                offset_x,
                offset_y,
            ) {
                Some(point) => point,
                None => return,
            };
            if is_erase {
                let removed_ids = {
                    let mut state = down_state.borrow_mut();
                    state.mode = Mode::Erase(EraseMode::Active {
                        hits: HashSet::new(),
                    });
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
                if let Mode::Draw(draw) = &mut state.mode {
                    draw.mode = DrawMode::Drawing { id: id.clone() };
                }
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
            let (pan_x, pan_y, zoom, offset_x, offset_y) = {
                let state = move_state.borrow();
                (
                    state.pan_x,
                    state.pan_y,
                    state.zoom,
                    state.board_offset_x,
                    state.board_offset_y,
                )
            };
            let rect = move_canvas.get_bounding_client_rect();
            let screen_x = event.client_x() as f64 - rect.left();
            let screen_y = event.client_y() as f64 - rect.top();
            let hit = {
                let state = move_state.borrow();
                match &state.mode {
                    Mode::Select(select) => selection_hit_test(&state, select, screen_x, screen_y),
                    _ => None,
                }
            };
            let mut state = move_state.borrow_mut();
            match &mut state.mode {
                Mode::Select(select) => {
                    let world_point = match event_to_point(
                        &move_canvas,
                        &event,
                        pan_x,
                        pan_y,
                        zoom,
                        offset_x,
                        offset_y,
                    ) {
                        Some(point) => point,
                        None => return,
                    };
                    match &mut select.mode {
                        SelectMode::Lasso { points } => {
                            points.push(world_point);
                            redraw(&mut state);
                        }
                        SelectMode::Move { start, snapshot } => {
                            let delta_x = world_point.x - start.x;
                            let delta_y = world_point.y - start.y;
                            let updated = apply_translation(snapshot, delta_x, delta_y);
                            apply_transformed_strokes(&mut state, &updated);
                            for stroke in updated {
                                send_message(&move_socket, &ClientMessage::StrokeReplace { stroke });
                            }
                        }
                        SelectMode::Scale {
                            anchor,
                            start,
                            axis,
                            snapshot,
                        } => {
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
                            let updated = apply_scale_xy(snapshot, *anchor, sx, sy);
                            apply_transformed_strokes(&mut state, &updated);
                            for stroke in updated {
                                send_message(&move_socket, &ClientMessage::StrokeReplace { stroke });
                            }
                        }
                        SelectMode::Rotate {
                            center,
                            start_angle,
                            snapshot,
                        } => {
                            let angle = angle_between(*center, world_point);
                            let delta = angle - *start_angle;
                            let updated = apply_rotation(snapshot, *center, delta);
                            apply_transformed_strokes(&mut state, &updated);
                            for stroke in updated {
                                send_message(&move_socket, &ClientMessage::StrokeReplace { stroke });
                            }
                        }
                        SelectMode::Idle => {
                            if hit.is_some() {
                                set_canvas_mode(&state.canvas, &state.mode, false);
                            }
                        }
                    }
                }
                Mode::Erase(EraseMode::Active { .. }) => {
                    let point = match event_to_point(
                        &move_canvas,
                        &event,
                        pan_x,
                        pan_y,
                        zoom,
                        offset_x,
                        offset_y,
                    ) {
                        Some(point) => point,
                        None => return,
                    };
                    let removed_ids = erase_hits_at_point(&mut state, point);
                    for id in removed_ids {
                        send_message(&move_socket, &ClientMessage::Erase { id });
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
                    redraw(&mut state);
                }
                Mode::Draw(draw) => {
                    let id = match &draw.mode {
                        DrawMode::Drawing { id } => id.clone(),
                        _ => return,
                    };
                    let point = match event_to_point(
                        &move_canvas,
                        &event,
                        pan_x,
                        pan_y,
                        zoom,
                        offset_x,
                        offset_y,
                    ) {
                        Some(point) => point,
                        None => return,
                    };
                    if let Some(last_point) = last_point_for_id(&state.strokes, &id) {
                        if last_point == point {
                            return;
                        }
                    }
                    move_stroke(&mut state, &id, point);
                    send_message(&move_socket, &ClientMessage::StrokeMove { id, point });
                }
                _ => {}
            }
        });
        canvas.add_event_listener_with_callback("pointermove", onmove.as_ref().unchecked_ref())?;
        onmove.forget();
    }

    {
        let stop_state = state.clone();
        let stop_socket = socket.clone();
        let stop_canvas = canvas.clone();
        let onstop = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            let mut state = stop_state.borrow_mut();
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
            match &mut state.mode {
                Mode::Select(select) => {
                    let end_ids = match select.mode {
                        SelectMode::Move { .. }
                        | SelectMode::Scale { .. }
                        | SelectMode::Rotate { .. } => Some(select.selected_ids.clone()),
                        _ => None,
                    };
                    if matches!(select.mode, SelectMode::Lasso { .. }) {
                        finalize_lasso_selection(&mut state);
                    }
                    if let Mode::Select(select) = &mut state.mode {
                        select.mode = SelectMode::Idle;
                    }
                    redraw(&mut state);
                    drop(state);
                    if let Some(ids) = end_ids {
                        if !ids.is_empty() {
                            send_message(&stop_socket, &ClientMessage::TransformEnd { ids });
                        }
                    }
                }
                Mode::Erase(EraseMode::Active { .. }) => {
                    state.mode = Mode::Erase(EraseMode::Idle);
                }
                Mode::Pan(PanMode::Active { .. }) => {
                    state.mode = Mode::Pan(PanMode::Idle);
                    set_canvas_mode(&state.canvas, &state.mode, false);
                }
                Mode::Draw(draw) => {
                    let id = match &draw.mode {
                        DrawMode::Drawing { id } => id.clone(),
                        _ => return,
                    };
                    draw.mode = DrawMode::Idle;
                    end_stroke(&mut state, &id);
                    drop(state);
                    send_message(&stop_socket, &ClientMessage::StrokeEnd { id });
                }
                _ => {}
            }
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
            let (offset_x, offset_y, zoom, pan_x, pan_y) = {
                let state = zoom_state.borrow();
                (
                    state.board_offset_x,
                    state.board_offset_y,
                    state.zoom,
                    state.pan_x,
                    state.pan_y,
                )
            };
            let cursor_x = wheel_event.client_x() as f64 - rect.left();
            let cursor_y = wheel_event.client_y() as f64 - rect.top();
            let world_x = (cursor_x - pan_x - offset_x) / zoom;
            let world_y = (cursor_y - pan_y - offset_y) / zoom;
            let zoom_factor = if wheel_event.delta_y() < 0.0 {
                1.1
            } else {
                0.9
            };
            let next_zoom = (zoom * zoom_factor).clamp(0.4, 4.0);
            let next_pan_x = cursor_x - offset_x - world_x * next_zoom;
            let next_pan_y = cursor_y - offset_y - world_y * next_zoom;
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
