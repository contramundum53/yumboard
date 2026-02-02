use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, Event, MessageEvent, WebSocket, Window};

use yumboard_shared::{ClientMessage, ServerMessage};

use crate::net::websocket_url;

#[derive(Debug)]
pub enum WsEvent {
    Open,
    Close,
    Error,
    Message(ServerMessage),
}

pub struct WsSender {
    socket: WebSocket,
}

impl WsSender {
    pub fn is_open(&self) -> bool {
        self.socket.ready_state() == WebSocket::OPEN
    }

    pub fn send(&self, message: &ClientMessage) {
        if !self.is_open() {
            return;
        }
        if let Ok(payload) = bincode::encode_to_vec(message, bincode::config::standard()) {
            let _ = self.socket.send_with_u8_array(&payload);
        }
    }
}

fn window_user_agent(window: &Window) -> Option<String> {
    let navigator = Reflect::get(window.as_ref(), &JsValue::from_str("navigator")).ok()?;
    Reflect::get(&navigator, &JsValue::from_str("userAgent"))
        .ok()?
        .as_string()
}

fn navigator_max_touch_points(window: &Window) -> Option<u32> {
    let navigator = Reflect::get(window.as_ref(), &JsValue::from_str("navigator")).ok()?;
    Reflect::get(&navigator, &JsValue::from_str("maxTouchPoints"))
        .ok()?
        .as_f64()
        .map(|value| value as u32)
}

fn should_kick_safari_ws(window: &Window) -> bool {
    let ua = window_user_agent(window).unwrap_or_default();
    let is_safari = ua.contains("Safari")
        && !ua.contains("Chrome")
        && !ua.contains("CriOS")
        && !ua.contains("FxiOS")
        && !ua.contains("Edg")
        && !ua.contains("OPR");
    let touch = navigator_max_touch_points(window).unwrap_or(0) > 1;
    is_safari && touch
}

fn ping_url() -> String {
    let now = js_sys::Date::now() as u64;
    format!("/ping?t={now}")
}

pub fn connect_ws(
    window: &Window,
    on_event: impl 'static + FnMut(WsEvent),
) -> Result<Rc<WsSender>, JsValue> {
    let ws_url = websocket_url(window)?;
    let socket = WebSocket::new(&ws_url)?;
    let _ = Reflect::set(
        socket.as_ref(),
        &JsValue::from_str("binaryType"),
        &JsValue::from_str("arraybuffer"),
    );

    let sender = Rc::new(WsSender {
        socket: socket.clone(),
    });

    let on_event = Rc::new(RefCell::new(on_event));
    let open_reported = Rc::new(Cell::new(false));

    {
        let on_event = on_event.clone();
        let open_reported = open_reported.clone();
        let onopen = Closure::<dyn FnMut(Event)>::new(move |_| {
            open_reported.set(true);
            on_event.borrow_mut()(WsEvent::Open);
        });
        socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();
    }

    {
        let on_event = on_event.clone();
        let open_reported = open_reported.clone();
        let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |_| {
            open_reported.set(false);
            on_event.borrow_mut()(WsEvent::Close);
        });
        socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();
    }

    {
        let on_event = on_event.clone();
        let open_reported = open_reported.clone();
        let onerror = Closure::<dyn FnMut(Event)>::new(move |_| {
            open_reported.set(false);
            on_event.borrow_mut()(WsEvent::Error);
        });
        socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();
    }

    {
        let on_event = on_event.clone();
        let open_reported = open_reported.clone();
        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if !open_reported.replace(true) {
                on_event.borrow_mut()(WsEvent::Open);
            }

            let message = if let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
                let bytes = Uint8Array::new(&buffer).to_vec();
                match bincode::decode_from_slice::<ServerMessage, _>(
                    &bytes,
                    bincode::config::standard(),
                ) {
                    Ok((message, _)) => message,
                    Err(error) => {
                        web_sys::console::error_1(
                            &format!("WS message bincode parse error: {error}").into(),
                        );
                        return;
                    }
                }
            } else if let Some(text) = event.data().as_string() {
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(message) => message,
                    Err(error) => {
                        let snippet = if text.len() <= 200 {
                            text
                        } else {
                            format!("{}...", &text[..200])
                        };
                        web_sys::console::error_1(
                            &format!("WS message JSON parse error: {error} payload={snippet:?}")
                                .into(),
                        );
                        return;
                    }
                }
            } else {
                web_sys::console::error_2(
                    &"WS message data is not a string or arraybuffer".into(),
                    &event.data(),
                );
                return;
            };

            on_event.borrow_mut()(WsEvent::Message(message));
        });
        socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
    }

    if should_kick_safari_ws(window) {
        for delay_ms in [250, 6000] {
            let socket = socket.clone();
            let window_cb = window.clone();
            let onkick = Closure::<dyn FnMut()>::new(move || {
                if socket.ready_state() == WebSocket::CONNECTING {
                    let _ = window_cb.fetch_with_str(&ping_url());
                }
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                onkick.as_ref().unchecked_ref(),
                delay_ms,
            );
            onkick.forget();
        }
    }

    {
        let socket = socket.clone();
        let onbeforeunload = Closure::<dyn FnMut(Event)>::new(move |_| {
            let _ = socket.close();
        });
        window.add_event_listener_with_callback(
            "beforeunload",
            onbeforeunload.as_ref().unchecked_ref(),
        )?;
        onbeforeunload.forget();
    }

    Ok(sender)
}
