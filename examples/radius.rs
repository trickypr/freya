use dioxus::{core::UiEvent, events::MouseData, prelude::*};
use elements_namespace as dioxus_elements;

use trev::launch;

fn main() {
    launch(app);
}

fn app(cx: Scope) -> Element {
    let mut radius = use_state(&cx, || 30f32);

    let onscroll = move |ev: UiEvent<MouseData>| {
        let page = ev.coordinates().page();
        radius += (page.y as f32) * 20.0;
    };

    cx.render(rsx!(
        view {
            height: "100%",
            width: "100%",
            padding: "125",
            onscroll: onscroll,
            view {
                shadow: "0 0 150 30.0 black",
                radius: "{radius}",
                height: "100%",
                width: "100%",
                background: "black",
            }
        }
    ))
}