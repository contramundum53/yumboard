use yumboard_shared::StrokeId;

fn random_u32() -> u32 {
    (js_sys::Math::random() * (u32::MAX as f64 + 1.0)) as u32
}

fn random_u64() -> u64 {
    (u64::from(random_u32()) << 32) | u64::from(random_u32())
}

pub fn make_id() -> StrokeId {
    StrokeId::new([random_u64(), random_u64()])
}
