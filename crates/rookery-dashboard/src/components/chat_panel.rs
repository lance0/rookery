use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use crate::components::toast::{Toast, ToastKind, show_toast};

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

    let send_message = move || {
        let text = input.get().trim().to_string();
        if text.is_empty() || streaming.get() {
            return;
        }

        set_input.set(String::new());

        // Build messages payload BEFORE adding the empty assistant placeholder
        // Filter out any incomplete messages from previous errors
        let mut chat_msgs: Vec<serde_json::Value> = messages.get().iter()
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

        let set_toasts = set_toasts.clone();

        wasm_bindgen_futures::spawn_local(async move {
            match stream_chat(chat_msgs, set_messages).await {
                Ok(()) => {}
                Err(e) => {
                    show_toast(set_toasts, format!("chat error: {e}"), ToastKind::Error);
                    set_messages.update(|msgs| {
                        if let Some(last) = msgs.last() {
                            if last.role == "assistant" {
                                if last.content.is_empty() {
                                    // No content received — remove placeholder
                                    msgs.pop();
                                } else {
                                    // Partial content — mark as incomplete
                                    msgs.last_mut().unwrap().content.push_str(" [incomplete]");
                                }
                            }
                        }
                    });
                }
            }
            set_streaming.set(false);
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

    view! {
        <div class="chat-container">
            <div class="chat-messages" id="chat-scroll">
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
                    <button
                        class="btn"
                        on:click=on_send
                        disabled=move || streaming.get() || input.get().trim().is_empty()
                    >
                        {move || if streaming.get() { "..." } else { "Send" }}
                    </button>
                    <button
                        class="btn"
                        on:click=on_clear
                        disabled=move || streaming.get() || messages.get().is_empty()
                    >
                        "Clear"
                    </button>
                </div>
            </div>
        </div>
    }
}

async fn stream_chat(
    messages: Vec<serde_json::Value>,
    set_messages: WriteSignal<Vec<ChatMessage>>,
) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;

    let body = serde_json::json!({
        "messages": messages,
        "max_tokens": 2048,
    });

    let mut opts = web_sys::RequestInit::new();
    opts.set_method("POST");

    let headers = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| format!("{e:?}"))?;
    opts.set_headers(&headers);

    opts.set_body(&JsValue::from_str(&body.to_string()));

    let request =
        web_sys::Request::new_with_str_and_init("/api/chat", &opts).map_err(|e| format!("{e:?}"))?;

    let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("{e:?}"))?;

    let resp: web_sys::Response = resp_value.dyn_into().map_err(|_| "not a Response")?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let body = resp.body().ok_or("no body")?;
    let reader = body
        .get_reader()
        .dyn_into::<web_sys::ReadableStreamDefaultReader>()
        .map_err(|_| "not a reader")?;

    let decoder = js_sys::eval("new TextDecoder()").map_err(|e| format!("{e:?}"))?;

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
            break;
        }

        let value = js_sys::Reflect::get(&result, &JsValue::from_str("value"))
            .map_err(|e| format!("{e:?}"))?;

        // Decode Uint8Array to string
        let decode_fn = js_sys::Reflect::get(&decoder, &JsValue::from_str("decode"))
            .map_err(|e| format!("{e:?}"))?;
        let decode_fn: js_sys::Function = decode_fn.dyn_into().map_err(|_| "not a function")?;
        let text = decode_fn
            .call1(&decoder, &value)
            .map_err(|e| format!("{e:?}"))?;
        let chunk = text.as_string().unwrap_or_default();

        buffer.push_str(&chunk);

        // Process complete SSE lines
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();

            if line.starts_with("data: ") {
                let data = &line[6..];
                if data == "[DONE]" {
                    return Ok(());
                }

                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(content) = parsed
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                        .and_then(|c| c.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        let content = content.to_string();
                        set_messages.update(|msgs| {
                            if let Some(last) = msgs.last_mut() {
                                if last.role == "assistant" {
                                    last.content.push_str(&content);
                                }
                            }
                        });
                    }
                }
            }
        }
    }

    Ok(())
}
