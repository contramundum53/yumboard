use wasm_bindgen::JsValue;
use web_sys::{WebSocket, Window};

use yumboard_shared::ClientMessage;

pub fn websocket_url(window: &Window) -> Result<String, JsValue> {
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

pub fn send_message(socket: &WebSocket, message: &ClientMessage) {
    if socket.ready_state() == WebSocket::OPEN {
        if let Ok(payload) = serde_json::to_string(message) {
            let _ = socket.send_with_str(&payload);
        }
    }
}
