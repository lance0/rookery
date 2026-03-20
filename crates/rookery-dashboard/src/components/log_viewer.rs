use leptos::prelude::*;

#[component]
pub fn LogViewer(logs: ReadSignal<Vec<String>>) -> impl IntoView {
    let log_ref = NodeRef::<leptos::html::Div>::new();

    // Auto-scroll effect
    Effect::new(move |_| {
        let _lines = logs.get();
        if let Some(el) = log_ref.get() {
            let el: &web_sys::HtmlElement = &el;
            let at_bottom =
                el.scroll_top() + el.client_height() >= el.scroll_height() - 50;
            if at_bottom {
                el.set_scroll_top(el.scroll_height());
            }
        }
    });

    view! {
        <div class="card">
            <h2>"Logs"</h2>
            <div class="log-viewer" node_ref=log_ref>
                {move || logs.get().into_iter().map(|line| {
                    view! { <div class="log-line">{line}</div> }
                }).collect_view()}
            </div>
        </div>
    }
}
