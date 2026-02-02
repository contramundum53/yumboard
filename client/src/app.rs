use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Event, FileReader, HtmlAnchorElement, KeyboardEvent, PointerEvent, ProgressEvent};

use yumboard_shared::{ClientMessage, ServerMessage, Stroke, TransformOp};

use crate::actions::{
    adopt_strokes, apply_transform_operation, apply_transformed_strokes, clear_board, end_stroke,
    erase_hits_at_point, finalize_lasso_selection, move_stroke, parse_color, remove_stroke,
    replace_stroke_local, restore_stroke, sanitize_size, start_stroke,
};
use crate::dom::{coalesced_pointer_events, event_to_point, is_touch_event, resize_canvas, Ui};
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

fn pinch_distance(points: &[(f64, f64)]) -> f64 {
    let dx = points[0].0 - points[1].0;
    let dy = points[0].1 - points[1].1;
    (dx * dx + dy * dy).sqrt()
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
    let ui = Rc::new(Ui::from_document(document)?);

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

    ui.update_size_label();
    ui.set_status("connecting", "Connecting...");
    ui.set_tool_button(&ui.lasso_button, false);
    ui.set_tool_button(&ui.eraser_button, false);
    ui.set_tool_button(&ui.pan_button, false);
    ui.set_canvas_mode(&state.borrow().mode, false);
    {
        let state = state.borrow();
        let selected = palette_selected(&state.mode);
        if let Some(index) = selected {
            if let Some(color) = state.palette.get(index).cloned() {
                ui.color_input.set_value(&color);
            }
        }
        render_palette(&ui.document, &ui.palette_el, &state.palette, selected);
        ui.show_color_input(selected);
    }

    {
        let window = window.clone();
        let onclick = Closure::<dyn FnMut()>::new(move || {
            let _ = window.location().reload();
        });
        ui.reload_button
            .set_onclick(Some(onclick.as_ref().unchecked_ref()));
        onclick.forget();
    }

    let ws_offline_prompted = Rc::new(std::cell::Cell::new(false));
    let ws_sender = connect_ws(&window, {
        let ui = ui.clone();
        let message_state = state.clone();
        let ws_offline_prompted = ws_offline_prompted.clone();
        move |event: WsEvent| match event {
            WsEvent::Open => {
                ui.set_status("open", "Live connection");
            }
            WsEvent::Close => {
                ui.set_status("closed", "Offline");
                if !ws_offline_prompted.replace(true) {
                    ui.show_reload_banner("Connection lost. Please reload the page.");
                }
            }
            WsEvent::Error => {
                ui.set_status("closed", "Connection error");
                if !ws_offline_prompted.replace(true) {
                    ui.show_reload_banner("Connection error. Please reload the page.");
                }
            }
            WsEvent::Message(message) => {
                let mut state = message_state.borrow_mut();
                match message {
                    ServerMessage::Sync { strokes } => {
                        adopt_strokes(&mut state, &ui.ctx, strokes);
                    }
                    ServerMessage::StrokeStart {
                        id,
                        color,
                        size,
                        point,
                    } => {
                        start_stroke(&mut state, &ui.ctx, id, color, size, point);
                    }
                    ServerMessage::StrokeMove { id, point } => {
                        let _ = move_stroke(&mut state, &ui.ctx, &id, point);
                    }
                    ServerMessage::StrokePoints { id, points } => {
                        for point in points {
                            let _ = move_stroke(&mut state, &ui.ctx, &id, point);
                        }
                    }
                    ServerMessage::StrokeEnd { id } => {
                        end_stroke(&mut state, &id);
                    }
                    ServerMessage::Clear => {
                        clear_board(&mut state, &ui.ctx);
                    }
                    ServerMessage::StrokeRemove { id } => {
                        remove_stroke(&mut state, &id);
                        redraw(&ui.ctx, &mut state);
                    }
                    ServerMessage::StrokeRestore { stroke } => {
                        restore_stroke(&mut state, &ui.ctx, stroke);
                    }
                    ServerMessage::StrokeReplace { stroke } => {
                        replace_stroke_local(&mut state, stroke);
                        redraw(&ui.ctx, &mut state);
                    }
                    ServerMessage::TransformUpdate { ids, op } => {
                        apply_transform_operation(&mut state, &ui.ctx, &ids, &op);
                    }
                }
            }
        }
    })?;

    {
        let resize_state = state.clone();
        let window_cb = window.clone();
        let ui = ui.clone();
        let onresize = Closure::<dyn FnMut()>::new(move || {
            let mut state = resize_state.borrow_mut();
            resize_canvas(&window_cb, &ui.canvas, &ui.ctx, &mut state);
        });
        window.add_event_listener_with_callback("resize", onresize.as_ref().unchecked_ref())?;
        onresize.forget();
    }

    {
        let key_sender = ws_sender.clone();
        let key_state = state.clone();
        let ui_callback = ui.clone();
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
                        redraw(&ui_callback.ctx, &mut state);
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
        resize_canvas(&window, &ui.canvas, &ui.ctx, &mut state);
    }

    {
        let ui_callback = ui.clone();
        let oninput = Closure::<dyn FnMut(Event)>::new(move |_| {
            ui_callback.update_size_label();
        });
        ui.size_input
            .add_event_listener_with_callback("input", oninput.as_ref().unchecked_ref())?;
        oninput.forget();
    }

    {
        let tool_state = state.clone();
        let ui_callback = ui.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            if matches!(state.mode, Mode::Loading(_)) {
                return;
            }
            state.mode = Mode::Erase(EraseMode::Idle);
            ui_callback.sync_tool_ui(&state, false);
            render_palette(
                &ui_callback.document,
                &ui_callback.palette_el,
                &state.palette,
                palette_selected(&state.mode),
            );
            ui_callback.hide_color_input();
        });
        ui.eraser_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let tool_state = state.clone();
        let ui_callback = ui.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            if matches!(state.mode, Mode::Loading(_)) {
                return;
            }
            state.mode = Mode::Select(SelectState {
                selected_ids: Vec::new(),
                mode: SelectMode::Idle,
            });
            ui_callback.sync_tool_ui(&state, false);
            render_palette(
                &ui_callback.document,
                &ui_callback.palette_el,
                &state.palette,
                palette_selected(&state.mode),
            );
            ui_callback.hide_color_input();
        });
        ui.lasso_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let tool_state = state.clone();
        let ui_callback = ui.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = tool_state.borrow_mut();
            if matches!(state.mode, Mode::Loading(_)) {
                return;
            }
            state.mode = Mode::Pan(PanMode::Idle);
            ui_callback.sync_tool_ui(&state, false);
            render_palette(
                &ui_callback.document,
                &ui_callback.palette_el,
                &state.palette,
                palette_selected(&state.mode),
            );
            ui_callback.hide_color_input();
        });
        ui.pan_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let home_state = state.clone();
        let ui_callback = ui.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut state = home_state.borrow_mut();
            if matches!(state.mode, Mode::Loading(_)) {
                return;
            }
            let (zoom, pan_x, pan_y) = geometry::home_zoom_pan(&state);
            state.zoom = zoom;
            state.pan_x = pan_x;
            state.pan_y = pan_y;
            redraw(&ui_callback.ctx, &mut state);
        });
        ui.home_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let palette_state = state.clone();
        let ui_callback = ui.clone();
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
                    let color = ui_callback.color_input.value();
                    state.palette.push(color.clone());
                    let palette_selected = state.palette.len().saturating_sub(1);
                    state.mode = Mode::Draw(DrawState {
                        mode: DrawMode::Idle,
                        palette_selected,
                    });
                    ui_callback.color_input.set_value(&color);
                    ui_callback.sync_tool_ui(&state, false);
                    render_palette(
                        &ui_callback.document,
                        &ui_callback.palette_el,
                        &state.palette,
                        Some(palette_selected),
                    );
                    ui_callback.show_color_input(Some(palette_selected));
                    ui_callback.color_input.click();
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
                        ui_callback.color_input.set_value(&color);
                    }
                    ui_callback.sync_tool_ui(&state, false);
                    render_palette(
                        &ui_callback.document,
                        &ui_callback.palette_el,
                        &state.palette,
                        Some(index),
                    );
                    ui_callback.show_color_input(Some(index));
                    if already_selected {
                        ui_callback.color_input.click();
                    }
                }
            }
        });
        ui.palette_el
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let palette_state = state.clone();
        let ui_callback = ui.clone();
        let oninput = Closure::<dyn FnMut(Event)>::new(move |_| {
            let color = ui_callback.color_input.value();
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
                &ui_callback.document,
                &ui_callback.palette_el,
                &state.palette,
                palette_selected(&state.mode),
            );
            ui_callback.show_color_input(palette_selected(&state.mode));
        });
        ui.color_input
            .add_event_listener_with_callback("input", oninput.as_ref().unchecked_ref())?;
        oninput.forget();
    }

    {
        let clear_state = state.clone();
        let clear_sender = ws_sender.clone();
        let ui_callback = ui.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            if !clear_sender.is_open() {
                return;
            }
            {
                let mut state = clear_state.borrow_mut();
                clear_board(&mut state, &ui_callback.ctx);
            }
            clear_sender.send(&ClientMessage::Clear);
        });
        ui.clear_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
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
        ui.undo_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
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
        ui.redo_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let ui_callback = ui.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            event.stop_propagation();
            let is_open = !ui_callback.save_menu.has_attribute("hidden");
            if is_open {
                let _ = ui_callback.save_menu.set_attribute("hidden", "");
                let _ = ui_callback
                    .save_button
                    .set_attribute("aria-expanded", "false");
            } else {
                let _ = ui_callback.save_menu.remove_attribute("hidden");
                let _ = ui_callback
                    .save_button
                    .set_attribute("aria-expanded", "true");
            }
        });
        ui.save_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let save_state = state.clone();
        let ui_callback = ui.clone();
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
            if let Ok(element) = ui_callback.document.create_element("a") {
                if let Ok(anchor) = element.dyn_into::<HtmlAnchorElement>() {
                    anchor.set_href(&href);
                    anchor.set_download("yumboard.json");
                    anchor.click();
                }
            }
            let _ = ui_callback.save_menu.set_attribute("hidden", "");
            let _ = ui_callback
                .save_button
                .set_attribute("aria-expanded", "false");
        });
        ui.save_json_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let save_state = state.clone();
        let ui_callback = ui.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            let html = build_pdf_html(&save_state.borrow(), false);
            open_print_window(&ui_callback.document, &html);
            let _ = ui_callback.save_menu.set_attribute("hidden", "");
            let _ = ui_callback
                .save_button
                .set_attribute("aria-expanded", "false");
        });
        ui.save_pdf_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let ui_callback = ui.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let target: web_sys::EventTarget = match event.target() {
                Some(target) => target,
                None => return,
            };
            let Some(target) = target.dyn_into::<web_sys::Node>().ok() else {
                return;
            };
            let menu_node: web_sys::Node = ui_callback.save_menu.clone().into();
            let button_node: web_sys::Node = ui_callback.save_button.clone().into();
            if menu_node.contains(Some(&target)) || button_node.contains(Some(&target)) {
                return;
            }
            let _ = ui_callback.save_menu.set_attribute("hidden", "");
            let _ = ui_callback
                .save_button
                .set_attribute("aria-expanded", "false");
        });
        ui.document
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let ui_callback = ui.clone();
        let load_state = state.clone();
        let onclick = Closure::<dyn FnMut(Event)>::new(move |_| {
            if matches!(load_state.borrow().mode, Mode::Loading(_)) {
                return;
            }
            ui_callback.load_file.set_value("");
            ui_callback.load_file.click();
        });
        ui.load_button
            .add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref())?;
        onclick.forget();
    }

    {
        let ui_callback = ui.clone();
        let load_state_onchange = state.clone();
        let load_sender_onchange = ws_sender.clone();
        let onchange = Closure::<dyn FnMut(Event)>::new(move |_| {
            if !load_sender_onchange.is_open() {
                return;
            }
            let files = ui_callback.load_file.files();
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
            let ui_onload = ui_callback.clone();
            let onload = Closure::<dyn FnMut(ProgressEvent)>::new(move |event: ProgressEvent| {
                if !load_sender_onload.is_open() {
                    ui_onload.set_load_busy(false);
                    return;
                }
                let strokes = read_load_payload(&event);
                {
                    let mut state = load_state_onload.borrow_mut();
                    let Some(previous) = take_loading_previous(&mut state) else {
                        ui_onload.set_load_busy(false);
                        return;
                    };
                    state.mode = previous;
                    if let Some(strokes) = strokes.as_ref() {
                        adopt_strokes(&mut state, &ui_onload.ctx, strokes.clone());
                    }
                }
                ui_onload.set_load_busy(false);
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
            ui_callback.set_load_busy(true);
            let _ = reader.read_as_text(&file);
            let mut state = load_state_onchange.borrow_mut();
            if let Mode::Loading(loading) = &mut state.mode {
                loading.reader = Some(reader);
            }
        });
        ui.load_file
            .add_event_listener_with_callback("change", onchange.as_ref().unchecked_ref())?;
        onchange.forget();
    }

    {
        let down_state = state.clone();
        let down_sender = ws_sender.clone();
        let ui_callback = ui.clone();
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
                    let _ = ui_callback.canvas.set_pointer_capture(event.pointer_id());
                    return;
                }
                if state.touch_points.len() == 1 {
                    state.touch_pan = Some(PanMode::Active {
                        start_x: event.client_x() as f64,
                        start_y: event.client_y() as f64,
                        origin_x: state.pan_x,
                        origin_y: state.pan_y,
                    });
                    ui_callback.set_canvas_mode(&state.mode, true);
                    let _ = ui_callback.canvas.set_pointer_capture(event.pointer_id());
                    return;
                }
            }
            let rect = ui_callback.canvas.get_bounding_client_rect();
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
                    let world_point =
                        match event_to_point(&ui_callback.canvas, &event, pan_x, pan_y, zoom) {
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
                                redraw(&ui_callback.ctx, &mut state);
                                down_sender.send(&ClientMessage::Remove { ids });
                                let _ = ui_callback.canvas.set_pointer_capture(event.pointer_id());
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
                        let _ = ui_callback.canvas.set_pointer_capture(event.pointer_id());
                        return;
                    }
                    select.selected_ids.clear();
                    select.mode = SelectMode::Lasso {
                        points: vec![world_point],
                    };
                    state.mode = Mode::Select(select);
                    redraw(&ui_callback.ctx, &mut state);
                    let _ = ui_callback.canvas.set_pointer_capture(event.pointer_id());
                }
                Mode::Pan(_) => {
                    state.mode = Mode::Pan(PanMode::Active {
                        start_x: event.client_x() as f64,
                        start_y: event.client_y() as f64,
                        origin_x: pan_x,
                        origin_y: pan_y,
                    });
                    ui_callback.set_canvas_mode(&state.mode, true);
                    let _ = ui_callback.canvas.set_pointer_capture(event.pointer_id());
                }
                Mode::Erase(_) => {
                    if !down_sender.is_open() {
                        state.mode = Mode::Erase(EraseMode::Idle);
                        return;
                    }
                    let point =
                        match event_to_point(&ui_callback.canvas, &event, pan_x, pan_y, zoom) {
                            Some(point) => point,
                            None => {
                                state.mode = Mode::Erase(EraseMode::Idle);
                                return;
                            }
                        };
                    state.mode = Mode::Erase(EraseMode::Active {
                        hits: HashSet::new(),
                    });
                    let removed_ids = erase_hits_at_point(&mut state, &ui_callback.ctx, point);
                    for id in removed_ids {
                        down_sender.send(&ClientMessage::Erase { id });
                    }
                    let _ = ui_callback.canvas.set_pointer_capture(event.pointer_id());
                }
                Mode::Draw(mut draw) => {
                    if !down_sender.is_open() {
                        state.mode = Mode::Draw(draw);
                        return;
                    }
                    let point =
                        match event_to_point(&ui_callback.canvas, &event, pan_x, pan_y, zoom) {
                            Some(point) => point,
                            None => {
                                state.mode = Mode::Draw(draw);
                                return;
                            }
                        };
                    let id = make_id();
                    let color = parse_color(&ui_callback.color_input.value());
                    let size = sanitize_size(ui_callback.size_input.value_as_number() as f32);

                    state.active_draw_pointer = Some(event.pointer_id());
                    state.active_draw_timestamp = event.time_stamp();

                    draw.mode = DrawMode::Drawing { id: id.clone() };
                    state.mode = Mode::Draw(draw);
                    start_stroke(
                        &mut state,
                        &ui_callback.ctx,
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
                    let _ = ui_callback.canvas.set_pointer_capture(event.pointer_id());
                }
            }
        });
        ui.canvas
            .add_event_listener_with_callback("pointerdown", ondown.as_ref().unchecked_ref())?;
        ondown.forget();
    }

    {
        let move_state = state.clone();
        let move_sender = ws_sender.clone();
        let ui_callback = ui.clone();
        let move_window = window.clone();
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
                            redraw(&ui_callback.ctx, &mut state);
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
                            redraw(&ui_callback.ctx, &mut state);
                            continue;
                        }
                    }
                }
                let (pan_x, pan_y, zoom) = {
                    let state = move_state.borrow();
                    (state.pan_x, state.pan_y, state.zoom)
                };
                let rect = ui_callback.canvas.get_bounding_client_rect();
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
                            match event_to_point(&ui_callback.canvas, &event, pan_x, pan_y, zoom) {
                                Some(point) => point,
                                None => continue,
                            };
                        let selected_ids = select.selected_ids.clone();
                        let mut pending_update: Option<Vec<Stroke>> = None;
                        let mut pending_message: Option<ClientMessage> = None;
                        match &mut select.mode {
                            SelectMode::Lasso { points } => {
                                points.push(world_point);
                                redraw(&ui_callback.ctx, &mut state);
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
                                    ui_callback.set_canvas_mode(&state.mode, false);
                                }
                            }
                        }
                        if let Some(updated) = pending_update {
                            apply_transformed_strokes(&mut state, &ui_callback.ctx, &updated);
                        }
                        if let Some(message) = pending_message {
                            move_sender.send(&message);
                        }
                    }
                    Mode::Erase(EraseMode::Active { .. }) => {
                        if !move_sender.is_open() {
                            continue;
                        }
                        let point =
                            match event_to_point(&ui_callback.canvas, &event, pan_x, pan_y, zoom) {
                                Some(point) => point,
                                None => continue,
                            };
                        let removed_ids = erase_hits_at_point(&mut state, &ui_callback.ctx, point);
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
                        redraw(&ui_callback.ctx, &mut state);
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
                        let point =
                            match event_to_point(&ui_callback.canvas, &event, pan_x, pan_y, zoom) {
                                Some(point) => point,
                                None => continue,
                            };
                        if move_stroke(&mut state, &ui_callback.ctx, &id, point) {
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
        ui.canvas
            .add_event_listener_with_callback("pointermove", onmove.as_ref().unchecked_ref())?;
        ui.canvas.add_event_listener_with_callback(
            "pointerrawupdate",
            onmove.as_ref().unchecked_ref(),
        )?;
        onmove.forget();
    }

    {
        let stop_state = state.clone();
        let stop_sender = ws_sender.clone();
        let ui_callback = ui.clone();
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
                if ui_callback.canvas.has_pointer_capture(event.pointer_id()) {
                    let _ = ui_callback
                        .canvas
                        .release_pointer_capture(event.pointer_id());
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
            if ui_callback.canvas.has_pointer_capture(event.pointer_id()) {
                let _ = ui_callback
                    .canvas
                    .release_pointer_capture(event.pointer_id());
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
                    redraw(&ui_callback.ctx, &mut state);
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
                    ui_callback.set_canvas_mode(&state.mode, false);
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
        ui.canvas
            .add_event_listener_with_callback("pointerup", onstop.as_ref().unchecked_ref())?;
        ui.canvas
            .add_event_listener_with_callback("pointercancel", onstop.as_ref().unchecked_ref())?;
        ui.canvas
            .add_event_listener_with_callback("pointerleave", onstop.as_ref().unchecked_ref())?;
        ui.canvas.add_event_listener_with_callback(
            "lostpointercapture",
            onstop.as_ref().unchecked_ref(),
        )?;
        onstop.forget();
    }

    {
        let zoom_state = state.clone();
        let ui_callback = ui.clone();
        let onwheel = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let wheel_event = match event.dyn_into::<web_sys::WheelEvent>() {
                Ok(event) => event,
                Err(_) => return,
            };
            wheel_event.prevent_default();
            let rect = ui_callback.canvas.get_bounding_client_rect();
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
                redraw(&ui_callback.ctx, &mut state);
            }
        });
        ui.canvas
            .add_event_listener_with_callback("wheel", onwheel.as_ref().unchecked_ref())?;
        onwheel.forget();
    }

    Ok(())
}
