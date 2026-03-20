mod client;

use clap::{Parser, Subcommand};
use client::DaemonClient;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "rookery", version, about = "Local inference command center")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Daemon address
    #[arg(long, default_value = "http://127.0.0.1:3000", global = true)]
    daemon: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Start inference server with a profile
    Start {
        /// Profile name (uses default if omitted)
        profile: Option<String>,
    },
    /// Stop inference server
    Stop,
    /// Show server status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show GPU stats
    Gpu {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Hot-swap to a different profile
    Swap {
        /// Profile name to swap to
        profile: String,
    },
    /// List available profiles
    Profiles,
    /// View server logs
    Logs {
        /// Follow mode — stream new lines
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show (default 50)
        #[arg(short, long, default_value = "50")]
        n: usize,
    },
    /// Validate config file
    #[command(name = "config")]
    ConfigValidate,
    /// Manage agents
    Agent {
        #[command(subcommand)]
        cmd: AgentCommands,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Start an agent
    Start {
        /// Agent name (as configured in config.toml)
        name: String,
    },
    /// Stop an agent
    Stop {
        /// Agent name
        name: String,
    },
    /// List agents and their status
    Status,
}

// Response types matching daemon API
#[derive(Deserialize)]
struct StatusResponse {
    state: String,
    profile: Option<String>,
    pid: Option<u32>,
    port: Option<u16>,
    uptime_secs: Option<i64>,
}

#[derive(Deserialize)]
struct GpuResponse {
    gpus: Vec<GpuStats>,
}

#[derive(Deserialize)]
struct GpuStats {
    index: u32,
    name: String,
    vram_used_mb: u64,
    vram_total_mb: u64,
    temperature_c: u32,
    utilization_pct: u32,
    power_watts: f32,
    power_limit_watts: f32,
}

#[derive(Deserialize)]
struct ActionResponse {
    success: bool,
    message: String,
    status: StatusResponse,
}

#[derive(Serialize)]
struct StartRequest {
    profile: Option<String>,
}

#[derive(Serialize)]
struct EmptyBody {}

#[derive(Deserialize)]
struct AgentsResponse {
    agents: Vec<AgentInfo>,
    configured: Vec<String>,
}

#[derive(Deserialize)]
struct AgentInfo {
    name: String,
    pid: u32,
    started_at: String,
    status: serde_json::Value,
}

#[derive(Serialize)]
struct AgentActionRequest {
    name: String,
}

#[derive(Deserialize)]
struct AgentActionResponse {
    success: bool,
    message: String,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let client = DaemonClient::new(&cli.daemon);

    let result = match cli.command {
        Commands::Start { profile } => cmd_start(&client, profile).await,
        Commands::Stop => cmd_stop(&client).await,
        Commands::Status { json } => cmd_status(&client, json).await,
        Commands::Gpu { json } => cmd_gpu(&client, json).await,
        Commands::Swap { profile } => cmd_swap(&client, &profile).await,
        Commands::Profiles => cmd_profiles(&client).await,
        Commands::Logs { follow, n } => cmd_logs(&client, follow, n).await,
        Commands::ConfigValidate => cmd_config_validate().await,
        Commands::Agent { cmd } => match cmd {
            AgentCommands::Start { name } => cmd_agent_start(&client, &name).await,
            AgentCommands::Stop { name } => cmd_agent_stop(&client, &name).await,
            AgentCommands::Status => cmd_agent_status(&client).await,
        },
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn cmd_start(client: &DaemonClient, profile: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running (start it with `rookeryd`)".into());
    }

    let label = profile.as_deref().unwrap_or("default");
    println!("starting profile '{label}'...");

    let resp: ActionResponse = client
        .post("/api/start", &StartRequest { profile })
        .await?;

    if resp.success {
        println!("{}", resp.message);
        if let Some(pid) = resp.status.pid {
            println!("  PID:  {pid}");
        }
        if let Some(port) = resp.status.port {
            println!("  API:  http://localhost:{port}/v1");
        }
    } else {
        eprintln!("{}", resp.message);
    }

    Ok(())
}

async fn cmd_stop(client: &DaemonClient) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    println!("stopping server...");
    let resp: ActionResponse = client.post("/api/stop", &EmptyBody {}).await?;
    println!("{}", resp.message);
    Ok(())
}

async fn cmd_status(client: &DaemonClient, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        if json {
            println!(r#"{{"state":"daemon_offline"}}"#);
        } else {
            println!("rookeryd: offline");
        }
        return Ok(());
    }

    let resp: StatusResponse = client.get("/api/status").await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "state": resp.state,
            "profile": resp.profile,
            "pid": resp.pid,
            "port": resp.port,
            "uptime_secs": resp.uptime_secs,
        }))?);
    } else {
        println!("state:   {}", resp.state);
        if let Some(profile) = &resp.profile {
            println!("profile: {profile}");
        }
        if let Some(pid) = resp.pid {
            println!("pid:     {pid}");
        }
        if let Some(port) = resp.port {
            println!("port:    {port}");
            println!("api:     http://localhost:{port}/v1");
        }
        if let Some(uptime) = resp.uptime_secs {
            let hours = uptime / 3600;
            let mins = (uptime % 3600) / 60;
            let secs = uptime % 60;
            println!("uptime:  {hours}h {mins}m {secs}s");
        }
    }

    Ok(())
}

async fn cmd_gpu(client: &DaemonClient, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: GpuResponse = client.get("/api/gpu").await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "gpus": resp.gpus.iter().map(|g| serde_json::json!({
            "index": g.index,
            "name": g.name,
            "vram_used_mb": g.vram_used_mb,
            "vram_total_mb": g.vram_total_mb,
            "temperature_c": g.temperature_c,
            "utilization_pct": g.utilization_pct,
            "power_watts": g.power_watts,
            "power_limit_watts": g.power_limit_watts,
        })).collect::<Vec<_>>() }))?);
    } else {
        for gpu in &resp.gpus {
            println!("GPU {}: {}", gpu.index, gpu.name);
            println!(
                "  VRAM:  {} / {} MB ({:.0}%)",
                gpu.vram_used_mb,
                gpu.vram_total_mb,
                gpu.vram_used_mb as f64 / gpu.vram_total_mb as f64 * 100.0
            );
            println!("  Temp:  {}C", gpu.temperature_c);
            println!("  Util:  {}%", gpu.utilization_pct);
            println!(
                "  Power: {:.0}W / {:.0}W",
                gpu.power_watts, gpu.power_limit_watts
            );
        }
    }

    Ok(())
}

async fn cmd_config_validate() -> Result<(), Box<dyn std::error::Error>> {
    let config = rookery_core::config::Config::load()?;

    match config.validate() {
        Ok(()) => println!("config OK: {}", rookery_core::config::Config::config_path().display()),
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(1);
        }
    }

    println!("\nprofiles:");
    for (name, _profile) in &config.profiles {
        let args = config.resolve_command_line(name)?;
        let default_marker = if name == &config.default_profile {
            " (default)"
        } else {
            ""
        };
        println!("  {name}{default_marker}");
        println!("    {}", args.join(" \\\n      "));
        println!();
    }

    if !config.agents.is_empty() {
        println!("agents:");
        for (name, agent) in &config.agents {
            let auto = if agent.auto_start { " (auto-start)" } else { "" };
            println!("  {name}{auto}");
            println!("    {} {}", agent.command, agent.args.join(" "));
            println!();
        }
    }

    Ok(())
}

async fn cmd_agent_start(client: &DaemonClient, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    println!("starting agent '{name}'...");
    let resp: AgentActionResponse = client
        .post("/api/agents/start", &AgentActionRequest { name: name.to_string() })
        .await?;

    if resp.success {
        println!("{}", resp.message);
    } else {
        eprintln!("{}", resp.message);
    }

    Ok(())
}

async fn cmd_agent_stop(client: &DaemonClient, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    println!("stopping agent '{name}'...");
    let resp: AgentActionResponse = client
        .post("/api/agents/stop", &AgentActionRequest { name: name.to_string() })
        .await?;

    if resp.success {
        println!("{}", resp.message);
    } else {
        eprintln!("{}", resp.message);
    }

    Ok(())
}

async fn cmd_agent_status(client: &DaemonClient) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: AgentsResponse = client.get("/api/agents").await?;

    if resp.agents.is_empty() && resp.configured.is_empty() {
        println!("no agents configured");
        return Ok(());
    }

    println!("configured: {}", resp.configured.join(", "));

    if resp.agents.is_empty() {
        println!("running:    none");
    } else {
        println!();
        for agent in &resp.agents {
            let status = match &agent.status {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Object(m) => {
                    if let Some(err) = m.get("error") {
                        format!("failed: {}", err.as_str().unwrap_or("unknown"))
                    } else {
                        "unknown".into()
                    }
                }
                _ => "unknown".into(),
            };
            println!("  {} (PID {}) — {}", agent.name, agent.pid, status);
        }
    }

    Ok(())
}

async fn cmd_swap(client: &DaemonClient, profile: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    println!("swapping to '{profile}'...");
    let resp: ActionResponse = client
        .post("/api/swap", &serde_json::json!({ "profile": profile }))
        .await?;

    if resp.success {
        println!("{}", resp.message);
        if let Some(pid) = resp.status.pid {
            println!("  PID:  {pid}");
        }
        if let Some(port) = resp.status.port {
            println!("  API:  http://localhost:{port}/v1");
        }
    } else {
        eprintln!("{}", resp.message);
    }

    Ok(())
}

async fn cmd_profiles(client: &DaemonClient) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get("/api/profiles").await?;
    let profiles = resp["profiles"].as_array().unwrap();

    for p in profiles {
        let name = p["name"].as_str().unwrap_or("?");
        let model = p["model"].as_str().unwrap_or("?");
        let ctx = p["ctx_size"].as_u64().unwrap_or(0);
        let reasoning = p["reasoning_budget"].as_i64().unwrap_or(0);
        let is_default = p["default"].as_bool().unwrap_or(false);
        let vram = p["estimated_vram_mb"].as_u64();

        let default_marker = if is_default { " (default)" } else { "" };
        let thinking = if reasoning != 0 { " thinking" } else { "" };
        let ctx_label = if ctx >= 1024 {
            format!("{}K", ctx / 1024)
        } else {
            ctx.to_string()
        };

        print!("  {name}{default_marker} — {model}, {ctx_label} ctx{thinking}");
        if let Some(v) = vram {
            print!(", ~{:.1}GB VRAM", v as f64 / 1024.0);
        }
        println!();
    }

    Ok(())
}

async fn cmd_logs(client: &DaemonClient, follow: bool, n: usize) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    if !follow {
        // Fetch last N lines
        let resp: serde_json::Value = client.get(&format!("/api/logs?n={n}")).await?;
        if let Some(lines) = resp["lines"].as_array() {
            for line in lines {
                println!("{}", line.as_str().unwrap_or(""));
            }
        }
        return Ok(());
    }

    // Follow mode — connect to SSE and stream log lines
    println!("following logs (Ctrl+C to stop)...\n");

    let url = format!("{}/api/events", client.base_url());
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("failed to connect to SSE: {e}"))?;

    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;

    let mut buffer = String::new();
    let mut current_event = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Process complete SSE messages (double newline separated)
        while let Some(pos) = buffer.find("\n\n") {
            let message = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            for line in message.lines() {
                if let Some(event_type) = line.strip_prefix("event: ") {
                    current_event = event_type.to_string();
                } else if let Some(data) = line.strip_prefix("data: ") {
                    if current_event == "log" {
                        println!("{data}");
                    }
                }
            }
        }
    }

    Ok(())
}
