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

pub fn show_toast(
    set_toasts: WriteSignal<Vec<Toast>>,
    message: impl Into<String>,
    kind: ToastKind,
) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let toast = Toast {
        id,
        message: message.into(),
        kind,
    };
    let secs = match toast.kind {
        // Toasts are the only channel for swap/start/stop/save/bench errors, and
        // nothing else logs them — 3s was long enough to miss the whole failure.
        ToastKind::Error => 15,
        ToastKind::Success => 3,
    };
    set_toasts.update(|t| t.push(toast));

    wasm_bindgen_futures::spawn_local(async move {
        gloo_timers::future::sleep(std::time::Duration::from_secs(secs)).await;
        set_toasts.update(|t| t.retain(|toast| toast.id != id));
    });
}

#[component]
pub fn ToastContainer(
    toasts: ReadSignal<Vec<Toast>>,
    set_toasts: WriteSignal<Vec<Toast>>,
) -> impl IntoView {
    view! {
        // role="status" (polite): announced without interrupting. role="alert" is
        // assertive and would cut off whatever the reader is mid-sentence on, which
        // a "profile saved" toast does not warrant.
        <div class="toast-container" role="status">
            {move || {
                toasts.get().into_iter().map(|t| {
                    let class = match t.kind {
                        ToastKind::Success => "toast success",
                        ToastKind::Error => "toast error",
                    };
                    let id = t.id;
                    view! {
                        <div
                            class=class
                            title="click to dismiss"
                            on:click=move |_| set_toasts.update(|list| list.retain(|x| x.id != id))
                        >
                            {t.message}
                        </div>
                    }
                }).collect_view()
            }}
        </div>
    }
}
