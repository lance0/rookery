use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;

use crate::app_state::AppState;

pub const MAX_SSE_CONNECTIONS: u64 = 16;

#[derive(Default)]
pub struct RuntimeMetrics {
    server_restarts: AtomicU64,
    canary_checks: AtomicU64,
    canary_failures: AtomicU64,
    canary_restarts: AtomicU64,
    canary_last_check_timestamp: AtomicU64,
    chat_requests: AtomicU64,
    chat_errors: AtomicU64,
    chat_stream_timeouts: AtomicU64,
    sse_connections_current: AtomicU64,
    sse_connections_total: AtomicU64,
}

impl RuntimeMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc_server_restart(&self) {
        self.server_restarts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_canary_check(&self) {
        self.canary_checks.fetch_add(1, Ordering::Relaxed);
        self.canary_last_check_timestamp
            .store(Utc::now().timestamp().max(0) as u64, Ordering::Relaxed);
    }

    pub fn inc_canary_failure(&self) {
        self.canary_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_canary_restart(&self) {
        self.canary_restarts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_chat_request(&self) {
        self.chat_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_chat_error(&self) {
        self.chat_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_chat_stream_timeout(&self) {
        self.chat_stream_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn try_acquire_sse_connection(&self) -> bool {
        let previous = self.sse_connections_current.fetch_add(1, Ordering::Relaxed);
        if previous >= MAX_SSE_CONNECTIONS {
            self.sse_connections_current.fetch_sub(1, Ordering::Relaxed);
            return false;
        }

        self.sse_connections_total.fetch_add(1, Ordering::Relaxed);
        true
    }

    pub fn release_sse_connection(&self) {
        let current = self.sse_connections_current.load(Ordering::Relaxed);
        if current > 0 {
            self.sse_connections_current.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn server_restarts(&self) -> u64 {
        self.server_restarts.load(Ordering::Relaxed)
    }

    fn canary_checks(&self) -> u64 {
        self.canary_checks.load(Ordering::Relaxed)
    }

    fn canary_failures(&self) -> u64 {
        self.canary_failures.load(Ordering::Relaxed)
    }

    fn canary_restarts(&self) -> u64 {
        self.canary_restarts.load(Ordering::Relaxed)
    }

    fn canary_last_check_timestamp(&self) -> u64 {
        self.canary_last_check_timestamp.load(Ordering::Relaxed)
    }

    fn chat_requests(&self) -> u64 {
        self.chat_requests.load(Ordering::Relaxed)
    }

    fn chat_errors(&self) -> u64 {
        self.chat_errors.load(Ordering::Relaxed)
    }

    fn chat_stream_timeouts(&self) -> u64 {
        self.chat_stream_timeouts.load(Ordering::Relaxed)
    }

    pub(crate) fn sse_connections_current_value(&self) -> u64 {
        self.sse_connections_current.load(Ordering::Relaxed)
    }

    pub(crate) fn sse_connections_total_value(&self) -> u64 {
        self.sse_connections_total.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn set_sse_connections_current_for_test(&self, value: u64) {
        self.sse_connections_current.store(value, Ordering::SeqCst);
    }
}

pub(crate) struct SseConnectionGuard {
    metrics: Arc<RuntimeMetrics>,
}

impl SseConnectionGuard {
    pub fn new(metrics: Arc<RuntimeMetrics>) -> Self {
        Self { metrics }
    }
}

impl Drop for SseConnectionGuard {
    fn drop(&mut self) {
        self.metrics.release_sse_connection();
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct GpuLabels {
    gpu: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ServerLabels {
    profile: String,
    backend: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ProfileLabels {
    profile: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct AgentLabels {
    name: String,
}

pub async fn encode_metrics(state: &AppState) -> String {
    let mut registry = Registry::default();

    let gpu_vram_used_bytes = Family::<GpuLabels, Gauge<u64, AtomicU64>>::default();
    let gpu_vram_total_bytes = Family::<GpuLabels, Gauge<u64, AtomicU64>>::default();
    let gpu_temperature_celsius = Family::<GpuLabels, Gauge<u64, AtomicU64>>::default();
    let gpu_utilization_percent = Family::<GpuLabels, Gauge<u64, AtomicU64>>::default();
    let gpu_power_watts = Family::<GpuLabels, Gauge<f64, AtomicU64>>::default();

    let server_up = Family::<ServerLabels, Gauge<u64, AtomicU64>>::default();
    let server_uptime_seconds = Family::<ProfileLabels, Gauge<u64, AtomicU64>>::default();
    let server_restarts = Counter::<u64, AtomicU64>::default();

    let canary_checks = Counter::<u64, AtomicU64>::default();
    let canary_failures = Counter::<u64, AtomicU64>::default();
    let canary_restarts = Counter::<u64, AtomicU64>::default();
    let canary_last_check_timestamp = Gauge::<u64, AtomicU64>::default();

    let agent_up = Family::<AgentLabels, Gauge<u64, AtomicU64>>::default();
    let agent_uptime_seconds = Family::<AgentLabels, Gauge<u64, AtomicU64>>::default();
    let agent_restarts = Family::<AgentLabels, Counter<u64, AtomicU64>>::default();
    let agent_errors = Family::<AgentLabels, Counter<u64, AtomicU64>>::default();
    let agent_lifetime_errors = Family::<AgentLabels, Counter<u64, AtomicU64>>::default();

    let chat_requests = Counter::<u64, AtomicU64>::default();
    let chat_errors = Counter::<u64, AtomicU64>::default();
    let chat_stream_timeouts = Counter::<u64, AtomicU64>::default();

    let sse_connections_current = Gauge::<u64, AtomicU64>::default();
    let sse_connections_total = Counter::<u64, AtomicU64>::default();

    if let Some(monitor) = &state.gpu_monitor
        && let Ok(gpus) = monitor.stats()
    {
        for gpu in gpus {
            let labels = GpuLabels {
                gpu: gpu.index.to_string(),
            };
            gpu_vram_used_bytes
                .get_or_create(&labels)
                .set(gpu.vram_used_mb * 1024 * 1024);
            gpu_vram_total_bytes
                .get_or_create(&labels)
                .set(gpu.vram_total_mb * 1024 * 1024);
            gpu_temperature_celsius
                .get_or_create(&labels)
                .set(gpu.temperature_c as u64);
            gpu_utilization_percent
                .get_or_create(&labels)
                .set(gpu.utilization_pct as u64);
            gpu_power_watts
                .get_or_create(&labels)
                .set(gpu.power_watts as f64);
        }
    }

    let server_state = state.current_state().await;
    match server_state {
        rookery_core::state::ServerState::Running {
            profile,
            since,
            backend_type,
            ..
        } => {
            server_up
                .get_or_create(&ServerLabels {
                    profile: profile.clone(),
                    backend: backend_type.to_string(),
                })
                .set(1);
            server_uptime_seconds
                .get_or_create(&ProfileLabels { profile })
                .set(Utc::now().signed_duration_since(since).num_seconds().max(0) as u64);
        }
        rookery_core::state::ServerState::Sleeping { profile, .. } => {
            server_up
                .get_or_create(&ServerLabels {
                    profile,
                    backend: String::new(),
                })
                .set(0);
        }
        _ => {
            server_up
                .get_or_create(&ServerLabels {
                    profile: String::new(),
                    backend: String::new(),
                })
                .set(0);
        }
    }

    server_restarts.inc_by(state.metrics.server_restarts());
    canary_checks.inc_by(state.metrics.canary_checks());
    canary_failures.inc_by(state.metrics.canary_failures());
    canary_restarts.inc_by(state.metrics.canary_restarts());
    canary_last_check_timestamp.set(state.metrics.canary_last_check_timestamp());
    chat_requests.inc_by(state.metrics.chat_requests());
    chat_errors.inc_by(state.metrics.chat_errors());
    chat_stream_timeouts.inc_by(state.metrics.chat_stream_timeouts());
    sse_connections_current.set(state.metrics.sse_connections_current_value());
    sse_connections_total.inc_by(state.metrics.sse_connections_total_value());

    let configured_agents: BTreeSet<String> = {
        let config = state.config.read().await;
        config.agents.keys().cloned().collect()
    };
    let tracked_agents: BTreeSet<String> = state
        .agent_manager
        .list()
        .await
        .into_iter()
        .map(|agent| agent.name)
        .collect();

    let all_agent_names: BTreeSet<String> =
        configured_agents.union(&tracked_agents).cloned().collect();
    for name in all_agent_names {
        let labels = AgentLabels { name: name.clone() };
        if let Some(health) = state.agent_manager.get_health(&name).await {
            let is_up = if health.status == rookery_engine::agent::AgentStatus::Running {
                1
            } else {
                0
            };
            agent_up.get_or_create(&labels).set(is_up);
            if let Some(uptime) = health.uptime_secs {
                agent_uptime_seconds
                    .get_or_create(&labels)
                    .set(uptime.max(0) as u64);
            }
            let restart_count = health.total_restarts.unwrap_or(0) as u64;
            let error_count = health.error_count.unwrap_or(0) as u64;
            let lifetime_errors = health.lifetime_errors.unwrap_or(0) as u64;
            agent_restarts.get_or_create(&labels).inc_by(restart_count);
            agent_errors.get_or_create(&labels).inc_by(error_count);
            agent_lifetime_errors
                .get_or_create(&labels)
                .inc_by(lifetime_errors);
        } else {
            agent_up.get_or_create(&labels).set(0);
        }
    }

    registry.register(
        "rookery_gpu_vram_used_bytes",
        "GPU VRAM currently used in bytes.",
        gpu_vram_used_bytes,
    );
    registry.register(
        "rookery_gpu_vram_total_bytes",
        "GPU VRAM total in bytes.",
        gpu_vram_total_bytes,
    );
    registry.register(
        "rookery_gpu_temperature_celsius",
        "GPU temperature in Celsius.",
        gpu_temperature_celsius,
    );
    registry.register(
        "rookery_gpu_utilization_percent",
        "GPU utilization percent.",
        gpu_utilization_percent,
    );
    registry.register(
        "rookery_gpu_power_watts",
        "GPU power draw in watts.",
        gpu_power_watts,
    );
    registry.register(
        "rookery_server_up",
        "Whether the inference server is running.",
        server_up,
    );
    registry.register(
        "rookery_server_uptime_seconds",
        "Inference server uptime in seconds.",
        server_uptime_seconds,
    );
    registry.register(
        "rookery_server_restarts",
        "Backend starts and restarts observed by this daemon process.",
        server_restarts,
    );
    registry.register(
        "rookery_canary_checks",
        "Total inference canary checks executed.",
        canary_checks,
    );
    registry.register(
        "rookery_canary_failures",
        "Total canary checks that entered failure retry flow.",
        canary_failures,
    );
    registry.register(
        "rookery_canary_restarts",
        "Total backend restarts initiated by the canary.",
        canary_restarts,
    );
    registry.register(
        "rookery_canary_last_check_timestamp",
        "Unix timestamp of the last canary check.",
        canary_last_check_timestamp,
    );
    registry.register("rookery_agent_up", "Whether an agent is up.", agent_up);
    registry.register(
        "rookery_agent_uptime_seconds",
        "Agent uptime in seconds.",
        agent_uptime_seconds,
    );
    registry.register(
        "rookery_agent_restarts",
        "Total agent restarts.",
        agent_restarts,
    );
    registry.register(
        "rookery_agent_errors",
        "Current runtime agent error count.",
        agent_errors,
    );
    registry.register(
        "rookery_agent_lifetime_errors",
        "Total lifetime agent errors.",
        agent_lifetime_errors,
    );
    registry.register(
        "rookery_chat_requests",
        "Total proxied chat requests.",
        chat_requests,
    );
    registry.register(
        "rookery_chat_errors",
        "Total chat proxy request errors.",
        chat_errors,
    );
    registry.register(
        "rookery_chat_stream_timeouts",
        "Total chat stream timeouts.",
        chat_stream_timeouts,
    );
    registry.register(
        "rookery_sse_connections_current",
        "Current SSE connections.",
        sse_connections_current,
    );
    registry.register(
        "rookery_sse_connections",
        "Total SSE connections accepted.",
        sse_connections_total,
    );

    let mut output = String::new();
    let _ = encode(&mut output, &registry);
    output
}

pub fn sse_connection_guard(metrics: Arc<RuntimeMetrics>) -> SseConnectionGuard {
    SseConnectionGuard::new(metrics)
}
