use crate::components::toast::{Toast, ToastKind, show_toast};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[component]
pub fn ChatPanel(set_toasts: WriteSignal<Vec<Toast>>) -> impl IntoView {
    let (messages, set_messages) = signal(Vec::<ChatMessage>::new());
    let (input, set_input) = signal(String::new());
    let (streaming, set_streaming) = signal(false);
    let (abort_ctrl, set_abort_ctrl) = signal(Option::<web_sys::AbortController>::None);
    // Screen-reader announcements. Deliberately holds *state* strings only — never
    // stream tokens, which would queue an announcement per delta (see chat-live-region
    // below). Empty until the first send so the region is silent at page load.
    let (announce, set_announce) = signal("");

    let chat_ref = NodeRef::<leptos::html::Div>::new();

    // Auto-scroll, same rule as LogViewer/AgentsTab: only follow the stream if the
    // user is already at the bottom, so scrolling up to read isn't yanked back.
    Effect::new(move |_| {
        let _msgs = messages.get();
        if let Some(el) = chat_ref.get() {
            let el: &web_sys::HtmlElement = &el;
            let at_bottom = el.scroll_top() + el.client_height() >= el.scroll_height() - 50;
            if at_bottom {
                el.set_scroll_top(el.scroll_height());
            }
        }
    });

    let send_message = move || {
        let text = input.get().trim().to_string();
        if text.is_empty() || streaming.get() {
            return;
        }

        set_input.set(String::new());

        // Build messages payload BEFORE adding the empty assistant placeholder
        // Filter out any incomplete messages from previous errors
        let mut chat_msgs: Vec<serde_json::Value> = messages
            .get()
            .iter()
            .filter(|m| !m.content.ends_with(" [incomplete]"))
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();
        chat_msgs.push(serde_json::json!({ "role": "user", "content": text.clone() }));

        set_messages.update(|msgs| {
            msgs.push(ChatMessage {
                role: "user".into(),
                content: text.clone(),
            });
            // Add empty assistant message to stream into
            msgs.push(ChatMessage {
                role: "assistant".into(),
                content: String::new(),
            });
        });
        set_streaming.set(true);
        set_announce.set("Response started.");

        // Create AbortController for this request
        let controller = web_sys::AbortController::new().ok();
        set_abort_ctrl.set(controller.clone());

        let set_toasts = set_toasts;

        wasm_bindgen_futures::spawn_local(async move {
            match stream_chat(chat_msgs, set_messages, controller.as_ref()).await {
                Ok(()) => set_announce.set("Response complete."),
                Err(e) => {
                    // Don't show toast for user-initiated aborts
                    if !e.contains("abort") && !e.contains("Abort") {
                        show_toast(set_toasts, format!("chat error: {e}"), ToastKind::Error);
                        set_announce.set("Response failed.");
                    } else {
                        set_announce.set("Response stopped.");
                    }
                    set_messages.update(|msgs| {
                        if let Some(last) = msgs.last()
                            && last.role == "assistant"
                        {
                            if last.content.is_empty() {
                                // No content received — remove placeholder
                                msgs.pop();
                            } else {
                                // Partial content — mark as incomplete
                                msgs.last_mut().unwrap().content.push_str(" [incomplete]");
                            }
                        }
                    });
                }
            }
            set_streaming.set(false);
            set_abort_ctrl.set(None);
        });
    };

    let on_send = move |_| send_message();

    let on_keydown = move |e: web_sys::KeyboardEvent| {
        if e.key() == "Enter" && !e.shift_key() {
            e.prevent_default();
            send_message();
        }
    };

    let on_clear = move |_| {
        if !streaming.get() {
            set_messages.set(Vec::new());
        }
    };

    let on_stop = move |_| {
        if let Some(ctrl) = abort_ctrl.get() {
            ctrl.abort();
        }
    };

    view! {
        <div class="chat-container">
            // No aria-live here on purpose: a token stream would queue one
            // announcement per delta and drown the screen reader. State is
            // announced by .chat-live-region below instead.
            <div class="chat-messages" id="chat-scroll" node_ref=chat_ref>
                {move || {
                    let msgs = messages.get();
                    if msgs.is_empty() {
                        view! {
                            <div class="chat-empty">"send a message to test the model"</div>
                        }.into_any()
                    } else {
                        view! {
                            <div>
                                {msgs.into_iter().map(|m| {
                                    let class = format!("chat-msg {}", m.role);
                                    let label = if m.role == "user" { "you" } else { "model" };
                                    view! {
                                        <div class=class>
                                            <div class="chat-msg-role">{label}</div>
                                            <div class="chat-msg-content">{m.content}</div>
                                        </div>
                                    }
                                }).collect_view()}
                            </div>
                        }.into_any()
                    }
                }}
            </div>
            <div class="chat-input-row">
                <textarea
                    class="chat-textarea"
                    placeholder="type a message..."
                    prop:value=move || input.get()
                    on:input=move |ev| set_input.set(event_target_value(&ev))
                    on:keydown=on_keydown
                    disabled=move || streaming.get()
                    rows="2"
                />
                <div class="chat-btn-col">
                    {move || if streaming.get() {
                        view! {
                            <button class="btn danger" on:click=on_stop>"Stop"</button>
                        }.into_any()
                    } else {
                        view! {
                            <button
                                class="btn"
                                on:click=on_send
                                disabled=move || input.get().trim().is_empty()
                            >
                                "Send"
                            </button>
                        }.into_any()
                    }}
                    <button
                        class="btn"
                        on:click=on_clear
                        disabled=move || streaming.get() || messages.get().is_empty()
                    >
                        "Clear"
                    </button>
                </div>
            </div>
            // Present at page load (every tab is mounted, hidden with display:none),
            // so the region is live before its text ever changes.
            <div class="chat-live-region" aria-live="polite" aria-atomic="true">
                {move || announce.get()}
            </div>
        </div>
    }
}

async fn stream_chat(
    messages: Vec<serde_json::Value>,
    set_messages: WriteSignal<Vec<ChatMessage>>,
    abort_controller: Option<&web_sys::AbortController>,
) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;

    let body = serde_json::json!({
        "messages": messages,
        "max_tokens": 2048,
    });

    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");

    let headers = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| format!("{e:?}"))?;
    if let Some(api_key) = crate::api::get_api_key() {
        headers
            .set("Authorization", &format!("Bearer {api_key}"))
            .map_err(|e| format!("{e:?}"))?;
    }
    opts.set_headers(&headers);

    opts.set_body(&JsValue::from_str(&body.to_string()));

    if let Some(ctrl) = abort_controller {
        opts.set_signal(Some(&ctrl.signal()));
    }

    let request = web_sys::Request::new_with_str_and_init("/api/chat", &opts)
        .map_err(|e| format!("{e:?}"))?;

    let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("{e:?}"))?;

    let resp: web_sys::Response = resp_value.dyn_into().map_err(|_| "not a Response")?;

    if !resp.ok() {
        if resp.status() == 401 {
            crate::api::notify_auth_required();
        }
        return Err(format!("HTTP {}", resp.status()));
    }

    let body = resp.body().ok_or("no body")?;
    let reader = body
        .get_reader()
        .dyn_into::<web_sys::ReadableStreamDefaultReader>()
        .map_err(|_| "not a reader")?;

    // One decoder for the whole stream, deliberately outside the read loop:
    // `stream: true` works by keeping the trailing bytes of a split UTF-8 sequence
    // inside the decoder until the next chunk arrives. A per-chunk decoder starts
    // with an empty internal buffer every time, so split sequences would still
    // decode to U+FFFD and the flag would buy nothing.
    let decoder = web_sys::TextDecoder::new().map_err(|e| format!("{e:?}"))?;
    let decode_opts = web_sys::TextDecodeOptions::new();
    decode_opts.set_stream(true);

    let mut buffer = String::new();

    loop {
        let result = wasm_bindgen_futures::JsFuture::from(reader.read())
            .await
            .map_err(|e| format!("{e:?}"))?;

        let done = js_sys::Reflect::get(&result, &JsValue::from_str("done"))
            .map_err(|e| format!("{e:?}"))?
            .as_bool()
            .unwrap_or(true);

        if done {
            // ponytail: no final `decoder.decode()` flush. Bytes still held by the
            // decoder here mean the server truncated a UTF-8 sequence, so the flush
            // could only yield U+FFFD — and it would land in `buffer`, whose trailing
            // partial line is discarded on break anyway. Unobservable either way.
            break;
        }

        let value: js_sys::Object = js_sys::Reflect::get(&result, &JsValue::from_str("value"))
            .map_err(|e| format!("{e:?}"))?
            .dyn_into()
            .map_err(|_| "stream chunk is not a buffer")?;

        let chunk = decoder
            .decode_with_buffer_source_and_options(&value, &decode_opts)
            .map_err(|e| format!("{e:?}"))?;

        buffer.push_str(&chunk);

        // Process complete SSE lines
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();

            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    return Ok(());
                }

                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data)
                    && let Some(content) = parsed
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                        .and_then(|c| c.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                {
                    let content = content.to_string();
                    set_messages.update(|msgs| {
                        if let Some(last) = msgs.last_mut()
                            && last.role == "assistant"
                        {
                            last.content.push_str(&content);
                        }
                    });
                }
            }
        }
    }

    Ok(())
}
