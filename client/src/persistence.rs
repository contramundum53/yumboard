use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, Event, HtmlIFrameElement};

use yumboard_shared::{decode_session_file, SessionFileData, Stroke};

use crate::state::{State, STROKE_UNIT};

pub fn parse_load_payload_bytes(bytes: &[u8]) -> Option<Vec<Stroke>> {
    if let Ok(SessionFileData { strokes }) = decode_session_file(bytes) {
        return Some(strokes);
    }
    let Ok(text) = String::from_utf8(bytes.to_vec()) else {
        return None;
    };
    parse_load_payload_text(&text)
}

pub fn parse_load_payload_text(text: &str) -> Option<Vec<Stroke>> {
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
    if let Ok(data) = serde_json::from_str::<SessionFileData>(text) {
        return Some(data.strokes);
    }
    #[derive(serde::Deserialize)]
    struct LegacySaveData {
        version: u8,
        strokes: Vec<Stroke>,
    }
    if let Ok(data) = serde_json::from_str::<LegacySaveData>(text) {
        let _ = data.version;
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

pub fn build_pdf_html(state: &State, include_background: bool) -> String {
    let (min_x, min_y, width, height) = pdf_bounds(state);
    let mut paths = String::new();
    for stroke in &state.strokes {
        if stroke.points.is_empty() {
            continue;
        }
        let mut data = String::new();
        for (index, point) in stroke.points.iter().enumerate() {
            let x = point.x as f64;
            let y = point.y as f64;
            if index == 0 {
                data.push_str(&format!("M {} {}", x, y));
            } else {
                data.push_str(&format!(" L {} {}", x, y));
            }
        }
        let color = stroke.color.to_rgba_css();
        let width = stroke.size as f64 * STROKE_UNIT;
        paths.push_str(&format!(
            "<path d=\"{}\" stroke=\"{}\" stroke-width=\"{}\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\" />",
            data, color, width
        ));
        if stroke.points.len() == 1 {
            let p = stroke.points[0];
            let cx = p.x as f64;
            let cy = p.y as f64;
            let r = width / 2.0;
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
        "<!doctype html><html><head><meta charset=\"utf-8\" /><style>@page{{margin:0;size:auto;}}html,body{{margin:0;padding:0;}}body{{display:block;}}svg{{display:block;width:100vw;height:100vh;}}</style></head><body><svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{min_x} {min_y} {width} {height}\" preserveAspectRatio=\"xMidYMid meet\">{background}{paths}</svg><script>window.onload=()=>{{window.print();}}</script></body></html>",
        min_x = min_x,
        min_y = min_y,
        width = width,
        height = height,
        background = background,
        paths = paths
    )
}

fn pdf_bounds(state: &State) -> (f64, f64, f64, f64) {
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    let mut max_size: f64 = 0.0;
    for stroke in &state.strokes {
        max_size = max_size.max(stroke.size as f64 * STROKE_UNIT);
        for point in &stroke.points {
            let x = point.x as f64;
            let y = point.y as f64;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if min_x == f64::MAX {
        return (0.0, 0.0, 1.0, 1.0);
    }
    let pad = (max_size / 2.0).max(1.0);
    min_x -= pad;
    min_y -= pad;
    max_x += pad;
    max_y += pad;
    let width = (max_x - min_x).max(1.0);
    let height = (max_y - min_y).max(1.0);
    (min_x, min_y, width, height)
}

pub fn open_print_window(document: &Document, html: &str) {
    let iframe: HtmlIFrameElement = match document
        .create_element("iframe")
        .ok()
        .and_then(|element| element.dyn_into::<HtmlIFrameElement>().ok())
    {
        Some(frame) => frame,
        None => return,
    };
    let _ = iframe.set_attribute(
        "style",
        "position:fixed;right:0;bottom:0;width:0;height:0;border:0;",
    );
    iframe.set_srcdoc(html);
    if let Some(body) = document.body() {
        let _ = body.append_child(&iframe);
    }
    let iframe_for_load = iframe.clone();
    let onload = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(window) = iframe_for_load.content_window() {
            let _ = window.focus();
            let _ = window.print();
        }
    });
    iframe.set_onload(Some(onload.as_ref().unchecked_ref()));
    onload.forget();
}
