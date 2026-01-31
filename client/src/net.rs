use wasm_bindgen::JsValue;
use web_sys::{WebSocket, Window};

use yumboard_shared::ClientMessage;

pub fn websocket_url(window: &Window) -> Result<String, JsValue> {
    let location = window.location();
    let protocol = location.protocol()?;
    let hostname = location.hostname()?;
    let port = location.port()?;
    let scheme = if protocol == "https:" { "wss" } else { "ws" };
    let host = if port.is_empty() {
        format_host(&hostname)
    } else {
        format!("{}:{}", format_host(&hostname), port)
    };
    let session_id = session_id_from_location(&location);
    if let Some(session_id) = session_id {
        Ok(format!("{scheme}://{host}/ws/{session_id}"))
    } else {
        Ok(format!("{scheme}://{host}/ws"))
    }
}

fn format_host(hostname: &str) -> String {
    if hostname.contains(':') && !hostname.starts_with('[') {
        format!("[{hostname}]")
    } else {
        hostname.to_string()
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

pub fn send_message(socket: &WebSocket, message: &ClientMessage) {
    if socket.ready_state() == WebSocket::OPEN {
        if let Ok(payload) = bincode::serde::encode_to_vec(message, bincode::config::standard()) {
            let _ = socket.send_with_u8_array(&payload);
        }
    }
}
