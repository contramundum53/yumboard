pub fn make_id() -> String {
    let rand = (js_sys::Math::random() * 1_000_000_000.0) as u64;
    let now = js_sys::Date::now() as u64;
    format!("{now:x}{rand:x}")
}
