use super::Gauge;
use crate::GpuData;
use leptos::prelude::*;

#[component]
pub fn GpuPanel(gpu: ReadSignal<GpuData>) -> impl IntoView {
    let gpu_name = move || {
        gpu.get()
            .gpus
            .first()
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "—".into())
    };

    let vram_used = Signal::derive(move || {
        gpu.get()
            .gpus
            .first()
            .map(|g| g.vram_used_mb as f64)
            .unwrap_or(0.0)
    });
    let vram_total = Signal::derive(move || {
        gpu.get()
            .gpus
            .first()
            .map(|g| g.vram_total_mb as f64)
            .unwrap_or(1.0)
    });
    let temp = Signal::derive(move || {
        gpu.get()
            .gpus
            .first()
            .map(|g| g.temperature_c as f64)
            .unwrap_or(0.0)
    });
    let temp_max = Signal::derive(move || 90.0);
    let util = Signal::derive(move || {
        gpu.get()
            .gpus
            .first()
            .map(|g| g.utilization_pct as f64)
            .unwrap_or(0.0)
    });
    let util_max = Signal::derive(move || 100.0);
    let power = Signal::derive(move || {
        gpu.get()
            .gpus
            .first()
            .map(|g| g.power_watts as f64)
            .unwrap_or(0.0)
    });
    let power_max = Signal::derive(move || {
        gpu.get()
            .gpus
            .first()
            .map(|g| g.power_limit_watts as f64)
            .unwrap_or(1.0)
    });

    let processes = move || {
        gpu.get()
            .gpus
            .first()
            .map(|g| g.processes.clone())
            .unwrap_or_default()
    };

    // The degraded marker is a property of the *payload*, not of the stream:
    // the SSE connection can be perfectly healthy — `ping`s on time, `ConnState
    // ::Live` — while NVML itself is failing. So it is read here, off `gpu`,
    // and never touches the connection state machine in `sse.rs`.
    //
    // Memoised so the gauges are only torn down when the flag actually flips,
    // not on every 2s tick.
    let error = Memo::new(move |_| gpu.get().error);

    view! {
        <div class="card">
            <h2>"GPU"</h2>
            {move || match error.get() {
                // No numbers to dim here — there are none. Say so instead of
                // rendering zeroes as current truth.
                Some(detail) => view! {
                    <div class="gpu-unavailable">
                        "GPU monitor unavailable"
                        <span class="gpu-unavailable-detail">{detail}</span>
                    </div>
                }.into_any(),
                None => view! {
                    <div>
                        <div class="stat">
                            <div class="stat-label">"Device"</div>
                            <div class="stat-value">{gpu_name}</div>
                        </div>

                        <Gauge label="VRAM" value=vram_used max=vram_total unit="MB" color="cyan" />
                        <Gauge label="Temp" value=temp max=temp_max unit="C" color="pink" />
                        <Gauge label="Util" value=util max=util_max unit="%" color="amber" />
                        <Gauge label="Power" value=power max=power_max unit="W" color="green" />

                        <div style="margin-top:12px">
                            <div class="stat-label">"Processes"</div>
                            {move || {
                                let procs = processes();
                                if procs.is_empty() {
                                    view! { <div class="empty">"none"</div> }.into_any()
                                } else {
                                    view! {
                                        <div>
                                            {procs.into_iter().map(|p| view! {
                                                <div class="process-row">
                                                    <span class="process-name">{p.name.clone()}" ("{p.pid}")"</span>
                                                    <span class="process-vram">{p.vram_mb}" MB"</span>
                                                </div>
                                            }).collect_view()}
                                        </div>
                                    }.into_any()
                                }
                            }}
                        </div>
                    </div>
                }.into_any(),
            }}
        </div>
    }
}
