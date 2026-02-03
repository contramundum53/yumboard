use wasm_bindgen::JsCast;
use web_sys::{Document, Element, Event, HtmlButtonElement, HtmlElement};

pub enum PaletteAction {
    Select(usize),
    Remove(usize),
    Add,
}

pub fn render_palette(
    document: &Document,
    palette_el: &HtmlElement,
    colors: &[String],
    selected: Option<usize>,
) {
    palette_el.set_inner_html("");
    for (index, color) in colors.iter().enumerate() {
        let Ok(wrapper_el) = document.create_element("div") else {
            continue;
        };
        let Ok(wrapper) = wrapper_el.dyn_into::<HtmlElement>() else {
            continue;
        };
        let _ = wrapper.set_attribute("class", "swatch-wrap");
        let Ok(element) = document.create_element("button") else {
            continue;
        };
        let Ok(button) = element.dyn_into::<HtmlButtonElement>() else {
            continue;
        };
        let _ = button.set_attribute("type", "button");
        let _ = button.set_attribute("data-index", &index.to_string());
        let _ = button.set_attribute("aria-label", &format!("Use color {color}"));
        let class_name = if selected == Some(index) {
            "swatch active"
        } else {
            "swatch"
        };
        let _ = button.set_attribute("class", class_name);
        let _ = button.style().set_property("background", color);
        let _ = wrapper.append_child(&button);
        if let Ok(remove_el) = document.create_element("button") {
            if let Ok(remove_button) = remove_el.dyn_into::<HtmlButtonElement>() {
                let _ = remove_button.set_attribute("type", "button");
                let _ = remove_button.set_attribute("data-action", "remove");
                let _ = remove_button.set_attribute("data-index", &index.to_string());
                let _ = remove_button.set_attribute("aria-label", "Remove palette color");
                let _ = remove_button.set_attribute("class", "swatch-remove");
                remove_button.set_inner_html(
                    "<svg viewBox=\"0 0 20 20\" aria-hidden=\"true\"><path d=\"M6 6l8 8M14 6l-8 8\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\"/></svg>",
                );
                let _ = wrapper.append_child(&remove_button);
            }
        }
        let _ = palette_el.append_child(&wrapper);
    }
    if let Ok(element) = document.create_element("button") {
        if let Ok(button) = element.dyn_into::<HtmlButtonElement>() {
            let _ = button.set_attribute("type", "button");
            let _ = button.set_attribute("data-action", "add");
            let _ = button.set_attribute("aria-label", "Add palette color");
            let _ = button.set_attribute("class", "swatch add-swatch");
            button.set_inner_html(
                "<svg viewBox=\"0 0 20 20\" aria-hidden=\"true\"><path d=\"M10 4v12M4 10h12\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\"/></svg>",
            );
            let _ = palette_el.append_child(&button);
        }
    }
}

pub fn palette_action_from_event(event: &Event) -> Option<PaletteAction> {
    let mut current = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok());
    while let Some(element) = current {
        if let Some(action) = element.get_attribute("data-action") {
            if action == "add" {
                return Some(PaletteAction::Add);
            }
            if action == "remove" {
                if let Some(index) = element.get_attribute("data-index") {
                    if let Ok(index) = index.parse::<usize>() {
                        return Some(PaletteAction::Remove(index));
                    }
                }
                return None;
            }
        }
        if let Some(index) = element.get_attribute("data-index") {
            if let Ok(index) = index.parse::<usize>() {
                return Some(PaletteAction::Select(index));
            }
            return None;
        }
        current = element.parent_element().map(|parent| parent.into());
    }
    None
}
