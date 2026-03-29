use nvml_wrapper::Nvml;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct GpuStats {
    pub index: u32,
    pub name: String,
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
    pub temperature_c: u32,
    pub utilization_pct: u32,
    pub power_watts: f32,
    pub power_limit_watts: f32,
    pub processes: Vec<GpuProcess>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuProcess {
    pub pid: u32,
    pub name: String,
    pub vram_mb: u64,
}

pub struct GpuMonitor {
    nvml: Nvml,
}

impl GpuMonitor {
    pub fn new() -> Result<Self, nvml_wrapper::error::NvmlError> {
        let nvml = Nvml::init()?;
        Ok(Self { nvml })
    }

    pub fn device(
        &self,
        index: u32,
    ) -> Result<nvml_wrapper::Device<'_>, nvml_wrapper::error::NvmlError> {
        self.nvml.device_by_index(index)
    }

    pub fn stats(&self) -> Result<Vec<GpuStats>, nvml_wrapper::error::NvmlError> {
        let count = self.nvml.device_count()?;
        let mut stats = Vec::with_capacity(count as usize);

        for i in 0..count {
            let device = self.nvml.device_by_index(i)?;

            let name = device.name()?;
            let memory = device.memory_info()?;
            let temp =
                device.temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)?;
            let util = device.utilization_rates()?;
            let power = device.power_usage().unwrap_or(0); // milliwatts
            let power_limit = device.enforced_power_limit().unwrap_or(0);

            // Enumerate compute processes on this GPU
            let processes = device
                .running_compute_processes()
                .unwrap_or_default()
                .into_iter()
                .map(|p| {
                    let name = process_name(p.pid);
                    let vram_mb = match p.used_gpu_memory {
                        nvml_wrapper::enums::device::UsedGpuMemory::Used(bytes) => {
                            bytes / (1024 * 1024)
                        }
                        _ => 0,
                    };
                    GpuProcess {
                        pid: p.pid,
                        name,
                        vram_mb,
                    }
                })
                .collect();

            stats.push(GpuStats {
                index: i,
                name,
                vram_used_mb: memory.used / (1024 * 1024),
                vram_total_mb: memory.total / (1024 * 1024),
                temperature_c: temp,
                utilization_pct: util.gpu,
                power_watts: power as f32 / 1000.0,
                power_limit_watts: power_limit as f32 / 1000.0,
                processes,
            });
        }

        Ok(stats)
    }

    /// Find orphan llama-server processes not tracked by rookery.
    pub fn find_orphan_llama_servers(&self, tracked_pid: Option<u32>) -> Vec<GpuProcess> {
        let mut orphans = Vec::new();
        if let Ok(stats) = self.stats() {
            for gpu in stats {
                for proc in gpu.processes {
                    if (proc.name.contains("llama-server") || proc.name.contains("llama_server"))
                        && tracked_pid != Some(proc.pid)
                    {
                        orphans.push(proc);
                    }
                }
            }
        }
        orphans
    }
}

/// Read process name from /proc/<pid>/comm
pub(crate) fn process_name(pid: u32) -> String {
    let comm_path = PathBuf::from(format!("/proc/{pid}/comm"));
    std::fs::read_to_string(&comm_path)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| format!("pid:{pid}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_name_returns_current_process() {
        let pid = std::process::id();
        let name = process_name(pid);
        // The test binary name should be something related to rookery_engine
        // At minimum, it should NOT fall back to "pid:N"
        assert!(
            !name.starts_with("pid:"),
            "process_name for our own PID should not fall back, got: {name}"
        );
        assert!(!name.is_empty());
    }

    #[test]
    fn test_process_name_fallback_for_nonexistent_pid() {
        let name = process_name(999_999_999);
        assert_eq!(name, "pid:999999999");
    }
}
