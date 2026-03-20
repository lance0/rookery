use leptos::prelude::*;
use crate::GpuData;
use super::Gauge;

#[component]
pub fn GpuPanel(gpu: ReadSignal<GpuData>) -> impl IntoView {
    let gpu_name = move || {
        gpu.get().gpus.first().map(|g| g.name.clone()).unwrap_or_else(|| "—".into())
    };

    let vram_used = Signal::derive(move || gpu.get().gpus.first().map(|g| g.vram_used_mb as f64).unwrap_or(0.0));
    let vram_total = Signal::derive(move || gpu.get().gpus.first().map(|g| g.vram_total_mb as f64).unwrap_or(1.0));
    let temp = Signal::derive(move || gpu.get().gpus.first().map(|g| g.temperature_c as f64).unwrap_or(0.0));
    let temp_max = Signal::derive(move || 90.0);
    let util = Signal::derive(move || gpu.get().gpus.first().map(|g| g.utilization_pct as f64).unwrap_or(0.0));
    let util_max = Signal::derive(move || 100.0);
    let power = Signal::derive(move || gpu.get().gpus.first().map(|g| g.power_watts as f64).unwrap_or(0.0));
    let power_max = Signal::derive(move || gpu.get().gpus.first().map(|g| g.power_limit_watts as f64).unwrap_or(1.0));

    let processes = move || gpu.get().gpus.first().map(|g| g.processes.clone()).unwrap_or_default();

    view! {
        <div class="card">
            <h2>"GPU"</h2>
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
    }
}
