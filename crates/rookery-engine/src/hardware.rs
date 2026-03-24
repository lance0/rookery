use serde::Serialize;

use crate::gpu::GpuMonitor;

#[derive(Debug, Clone, Serialize)]
pub struct HardwareProfile {
    pub gpu: Option<GpuProfile>,
    pub cpu: CpuProfile,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuProfile {
    pub name: String,
    pub vram_total_mb: u64,
    pub compute_capability: (u32, u32),
    pub memory_bandwidth_gbps: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuProfile {
    pub name: String,
    pub cores: u32,
    pub threads: u32,
    pub ram_total_mb: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PerfEstimate {
    pub estimated_gen_toks: f32,
    pub fit_mode: FitMode,
    pub gpu_layers_hint: Option<i32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    FullGpu,
    PartialOffload,
    CpuOnly,
    WontFit,
}

impl std::fmt::Display for FitMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FitMode::FullGpu => write!(f, "full GPU"),
            FitMode::PartialOffload => write!(f, "partial offload"),
            FitMode::CpuOnly => write!(f, "CPU only"),
            FitMode::WontFit => write!(f, "won't fit"),
        }
    }
}

pub fn build_hardware_profile(gpu_monitor: Option<&GpuMonitor>) -> HardwareProfile {
    let gpu = gpu_monitor.and_then(|m| {
        let stats = m.stats().ok()?;
        let first = stats.first()?;

        let device = m.device(0).ok()?;
        let cc = device.cuda_compute_capability().ok()?;

        let bandwidth = lookup_bandwidth(&first.name);

        Some(GpuProfile {
            name: first.name.clone(),
            vram_total_mb: first.vram_total_mb,
            compute_capability: (cc.major as u32, cc.minor as u32),
            memory_bandwidth_gbps: bandwidth,
        })
    });

    let cpu = read_cpu_profile();

    HardwareProfile { gpu, cpu }
}

/// Get live VRAM free from GPU monitor.
pub fn live_vram_free_mb(gpu_monitor: Option<&GpuMonitor>) -> u64 {
    gpu_monitor
        .and_then(|m| m.stats().ok())
        .and_then(|s| s.first().cloned())
        .map(|g| g.vram_total_mb - g.vram_used_mb)
        .unwrap_or(0)
}

/// Estimate performance for a given model size on this hardware.
pub fn estimate_performance(
    profile: &HardwareProfile,
    model_size_mb: u64,
    vram_free_mb: u64,
    ram_free_mb: u64,
) -> PerfEstimate {
    // Model size + ~15% for KV cache overhead
    let total_needed_mb = (model_size_mb as f64 * 1.15) as u64;

    let gpu = match &profile.gpu {
        Some(g) => g,
        None => {
            // CPU only — check RAM
            if ram_free_mb >= total_needed_mb {
                return PerfEstimate {
                    estimated_gen_toks: estimate_cpu_toks(model_size_mb),
                    fit_mode: FitMode::CpuOnly,
                    gpu_layers_hint: Some(0),
                };
            }
            return PerfEstimate {
                estimated_gen_toks: 0.0,
                fit_mode: FitMode::WontFit,
                gpu_layers_hint: None,
            };
        }
    };

    if vram_free_mb >= total_needed_mb {
        // Full GPU fit
        let toks = estimate_gpu_toks(gpu.memory_bandwidth_gbps, model_size_mb);
        PerfEstimate {
            estimated_gen_toks: toks,
            fit_mode: FitMode::FullGpu,
            gpu_layers_hint: Some(-1), // all layers on GPU
        }
    } else if vram_free_mb + ram_free_mb >= total_needed_mb {
        // Partial offload
        let gpu_fraction = vram_free_mb as f32 / total_needed_mb as f32;
        let estimated_layers = (gpu_fraction * 99.0) as i32; // rough: 99 layers typical
        let toks = estimate_gpu_toks(gpu.memory_bandwidth_gbps, model_size_mb) * gpu_fraction;
        PerfEstimate {
            estimated_gen_toks: toks,
            fit_mode: FitMode::PartialOffload,
            gpu_layers_hint: Some(estimated_layers),
        }
    } else {
        PerfEstimate {
            estimated_gen_toks: 0.0,
            fit_mode: FitMode::WontFit,
            gpu_layers_hint: None,
        }
    }
}

/// Rough gen tok/s estimate: bandwidth / bytes_per_token.
/// For GGUF Q4, ~0.5 bytes per param. Model size in MB already reflects quantization.
fn estimate_gpu_toks(bandwidth_gbps: f32, model_size_mb: u64) -> f32 {
    if model_size_mb == 0 {
        return 0.0;
    }
    // bandwidth in GB/s, model in MB
    // Each token reads ~model_size bytes from memory
    let model_size_gb = model_size_mb as f32 / 1024.0;
    bandwidth_gbps / model_size_gb
}

fn estimate_cpu_toks(model_size_mb: u64) -> f32 {
    // DDR5 ~80 GB/s typical, rough estimate
    let model_size_gb = model_size_mb as f32 / 1024.0;
    if model_size_gb > 0.0 {
        80.0 / model_size_gb
    } else {
        0.0
    }
}

/// Lookup memory bandwidth by GPU name. Fallback to conservative estimate.
fn lookup_bandwidth(name: &str) -> f32 {
    let name_lower = name.to_lowercase();
    if name_lower.contains("5090") {
        1792.0
    } else if name_lower.contains("5080") {
        960.0
    } else if name_lower.contains("5070 ti") {
        896.0
    } else if name_lower.contains("5070") {
        672.0
    } else if name_lower.contains("4090") {
        1008.0
    } else if name_lower.contains("4080") {
        717.0
    } else if name_lower.contains("4070 ti") {
        504.0
    } else if name_lower.contains("4070") {
        504.0
    } else if name_lower.contains("3090") {
        936.0
    } else if name_lower.contains("3080") {
        760.0
    } else if name_lower.contains("a100") {
        2039.0
    } else if name_lower.contains("h100") {
        3352.0
    } else if name_lower.contains("rtx pro 6000") || name_lower.contains("pro 6000") {
        1792.0
    } else {
        // Conservative fallback
        500.0
    }
}

fn read_cpu_profile() -> CpuProfile {
    let name = read_cpu_name().unwrap_or_else(|| "unknown".into());
    let (cores, threads) = read_cpu_counts();
    let ram_total_mb = read_ram_total_mb();

    CpuProfile {
        name,
        cores,
        threads,
        ram_total_mb,
    }
}

fn read_cpu_name() -> Option<String> {
    let content = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in content.lines() {
        if line.starts_with("model name") {
            return line.split(':').nth(1).map(|s| s.trim().to_string());
        }
    }
    None
}

fn read_cpu_counts() -> (u32, u32) {
    let content = match std::fs::read_to_string("/proc/cpuinfo") {
        Ok(c) => c,
        Err(_) => return (1, 1),
    };

    let threads = content.matches("processor").count() as u32;

    // Try to get physical cores from "cpu cores" field
    let cores = content
        .lines()
        .find(|l| l.starts_with("cpu cores"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(threads);

    (cores, threads.max(1))
}

fn read_ram_total_mb() -> u64 {
    let content = match std::fs::read_to_string("/proc/meminfo") {
        Ok(c) => c,
        Err(_) => return 0,
    };

    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            // Format: "MemTotal:       131702396 kB"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(kb) = parts[1].parse::<u64>() {
                    return kb / 1024;
                }
            }
        }
    }
    0
}

pub fn read_ram_free_mb() -> u64 {
    let content = match std::fs::read_to_string("/proc/meminfo") {
        Ok(c) => c,
        Err(_) => return 0,
    };

    for line in content.lines() {
        if line.starts_with("MemAvailable:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(kb) = parts[1].parse::<u64>() {
                    return kb / 1024;
                }
            }
        }
    }
    0
}
