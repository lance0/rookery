use leptos::prelude::*;

#[component]
pub fn Gauge(
    label: &'static str,
    #[prop(into)] value: Signal<f64>,
    #[prop(into)] max: Signal<f64>,
    unit: &'static str,
    color: &'static str,
) -> impl IntoView {
    let pct = move || {
        let m = max.get();
        if m > 0.0 {
            (value.get() / m * 100.0).min(100.0)
        } else {
            0.0
        }
    };

    let display = move || {
        format!("{:.0}{unit} / {:.0}{unit}", value.get(), max.get())
    };

    let bar_style = move || format!("width: {:.1}%", pct());

    view! {
        <div class="gauge">
            <div class="gauge-header">
                <span class="gauge-label">{label}</span>
                <span class="gauge-value">{display}</span>
            </div>
            <div class="gauge-track">
                <div class=format!("gauge-fill {color}") style=bar_style></div>
            </div>
        </div>
    }
}
