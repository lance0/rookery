use leptos::prelude::*;
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT_ID: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, PartialEq)]
pub enum ToastKind {
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u32,
    pub message: String,
    pub kind: ToastKind,
}

pub fn show_toast(set_toasts: WriteSignal<Vec<Toast>>, message: impl Into<String>, kind: ToastKind) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let toast = Toast {
        id,
        message: message.into(),
        kind,
    };
    set_toasts.update(|t| t.push(toast));

    // Auto-dismiss after 3s
    let set_toasts = set_toasts.clone();
    wasm_bindgen_futures::spawn_local(async move {
        gloo_timers::future::sleep(std::time::Duration::from_secs(3)).await;
        set_toasts.update(|t| t.retain(|toast| toast.id != id));
    });
}

#[component]
pub fn ToastContainer(toasts: ReadSignal<Vec<Toast>>) -> impl IntoView {
    view! {
        <div class="toast-container">
            {move || {
                toasts.get().into_iter().map(|t| {
                    let class = match t.kind {
                        ToastKind::Success => "toast success",
                        ToastKind::Error => "toast error",
                    };
                    view! {
                        <div class=class>{t.message}</div>
                    }
                }).collect_view()
            }}
        </div>
    }
}
