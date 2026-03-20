use nvml_wrapper::Nvml;
use serde::Serialize;

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
}

pub struct GpuMonitor {
    nvml: Nvml,
}

impl GpuMonitor {
    pub fn new() -> Result<Self, nvml_wrapper::error::NvmlError> {
        let nvml = Nvml::init()?;
        Ok(Self { nvml })
    }

    pub fn stats(&self) -> Result<Vec<GpuStats>, nvml_wrapper::error::NvmlError> {
        let count = self.nvml.device_count()?;
        let mut stats = Vec::with_capacity(count as usize);

        for i in 0..count {
            let device = self.nvml.device_by_index(i)?;

            let name = device.name()?;
            let memory = device.memory_info()?;
            let temp = device
                .temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)?;
            let util = device.utilization_rates()?;
            let power = device.power_usage().unwrap_or(0); // milliwatts
            let power_limit = device.enforced_power_limit().unwrap_or(0);

            stats.push(GpuStats {
                index: i,
                name,
                vram_used_mb: memory.used / (1024 * 1024),
                vram_total_mb: memory.total / (1024 * 1024),
                temperature_c: temp,
                utilization_pct: util.gpu,
                power_watts: power as f32 / 1000.0,
                power_limit_watts: power_limit as f32 / 1000.0,
            });
        }

        Ok(stats)
    }
}
