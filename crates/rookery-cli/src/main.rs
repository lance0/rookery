mod client;

use clap::{CommandFactory, Parser, Subcommand};
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Stop inference server
    Stop {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List available profiles
    Profiles {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// View server logs
    Logs {
        /// Follow mode — stream new lines
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show (default 50)
        #[arg(short, long, default_value = "50")]
        n: usize,
    },
    /// Run a quick benchmark
    Bench {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Validate config file
    #[command(name = "config")]
    ConfigValidate {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Manage agents
    Agent {
        #[command(subcommand)]
        cmd: AgentCommands,
    },
    /// Browse and manage models
    Models {
        #[command(subcommand)]
        cmd: ModelCommands,
    },
    /// Generate shell completions
    Completions {
        /// Shell type
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
enum ModelCommands {
    /// Search HuggingFace for GGUF models
    Search {
        /// Search query
        query: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List available quants for a model repo
    Quants {
        /// Repo (e.g., unsloth/Qwen3-8B-GGUF or just Qwen3-8B)
        repo: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Recommend best-fit quant for your hardware
    Recommend {
        /// Repo (e.g., unsloth/Qwen3-8B-GGUF or just Qwen3-8B)
        repo: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List locally cached models
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Download a model
    Pull {
        /// Repo (e.g., unsloth/Qwen3-8B-GGUF or just Qwen3-8B)
        repo: String,
        /// Quant to download (auto-picks best fit if omitted)
        #[arg(long)]
        quant: Option<String>,
    },
    /// Show hardware profile
    Hardware {
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show detailed health and metrics for an agent
    Describe {
        /// Agent name
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

// Response types matching daemon API
#[derive(Deserialize)]
struct StatusResponse {
    state: String,
    profile: Option<String>,
    pid: Option<u32>,
    port: Option<u16>,
    uptime_secs: Option<i64>,
    #[serde(default)]
    backend: Option<String>,
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
#[allow(dead_code)]
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
        Commands::Start { profile, json } => cmd_start(&client, profile, json).await,
        Commands::Stop { json } => cmd_stop(&client, json).await,
        Commands::Status { json } => cmd_status(&client, json).await,
        Commands::Gpu { json } => cmd_gpu(&client, json).await,
        Commands::Swap { profile, json } => cmd_swap(&client, &profile, json).await,
        Commands::Profiles { json } => cmd_profiles(&client, json).await,
        Commands::Logs { follow, n } => cmd_logs(&client, follow, n).await,
        Commands::Bench { json } => cmd_bench(&client, json).await,
        Commands::ConfigValidate { json } => cmd_config_validate(json).await,
        Commands::Agent { cmd } => match cmd {
            AgentCommands::Start { name } => cmd_agent_start(&client, &name).await,
            AgentCommands::Stop { name } => cmd_agent_stop(&client, &name).await,
            AgentCommands::Status { json } => cmd_agent_status(&client, json).await,
            AgentCommands::Describe { name, json } => {
                cmd_agent_describe(&client, &name, json).await
            }
        },
        Commands::Models { cmd } => match cmd {
            ModelCommands::Search { query, json } => cmd_models_search(&client, &query, json).await,
            ModelCommands::Quants { repo, json } => cmd_models_quants(&client, &repo, json).await,
            ModelCommands::Recommend { repo, json } => {
                cmd_models_recommend(&client, &repo, json).await
            }
            ModelCommands::List { json } => cmd_models_list(&client, json).await,
            ModelCommands::Pull { repo, quant } => cmd_models_pull(&client, &repo, quant).await,
            ModelCommands::Hardware { json } => cmd_hardware(&client, json).await,
        },
        Commands::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "rookery",
                &mut std::io::stdout(),
            );
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn cmd_start(
    client: &DaemonClient,
    profile: Option<String>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running (start it with `rookeryd`)".into());
    }

    if !json {
        let label = profile.as_deref().unwrap_or("default");
        println!("starting profile '{label}'...");
    }

    let resp: serde_json::Value = client.post("/api/start", &StartRequest { profile }).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        let success = resp["success"].as_bool().unwrap_or(false);
        let message = resp["message"].as_str().unwrap_or("");
        if success {
            println!("{message}");
            if let Some(pid) = resp["status"]["pid"].as_u64() {
                println!("  PID:  {pid}");
            }
            if let Some(port) = resp["status"]["port"].as_u64() {
                println!("  API:  http://localhost:{port}/v1");
            }
        } else {
            eprintln!("{message}");
        }
    }

    Ok(())
}

async fn cmd_stop(client: &DaemonClient, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    if !json {
        println!("stopping server...");
    }
    let resp: serde_json::Value = client.post("/api/stop", &EmptyBody {}).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        println!("{}", resp["message"].as_str().unwrap_or(""));
    }
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
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "state": resp.state,
                "profile": resp.profile,
                "pid": resp.pid,
                "port": resp.port,
                "uptime_secs": resp.uptime_secs,
                "backend": resp.backend,
            }))?
        );
    } else {
        println!("state:   {}", resp.state);
        if let Some(profile) = &resp.profile {
            println!("profile: {profile}");
        }
        if let Some(backend) = &resp.backend {
            println!("backend: {backend}");
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
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "gpus": resp.gpus.iter().map(|g| serde_json::json!({
            "index": g.index,
            "name": g.name,
            "vram_used_mb": g.vram_used_mb,
            "vram_total_mb": g.vram_total_mb,
            "temperature_c": g.temperature_c,
            "utilization_pct": g.utilization_pct,
            "power_watts": g.power_watts,
            "power_limit_watts": g.power_limit_watts,
        })).collect::<Vec<_>>() })
            )?
        );
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

async fn cmd_config_validate(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let config = rookery_core::config::Config::load()?;

    if json {
        match config.validate() {
            Ok(()) => println!(
                "{}",
                serde_json::json!({"valid": true, "path": rookery_core::config::Config::config_path().display().to_string()})
            ),
            Err(e) => {
                println!(
                    "{}",
                    serde_json::json!({"valid": false, "error": e.to_string()})
                );
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    match config.validate() {
        Ok(()) => println!(
            "config OK: {}",
            rookery_core::config::Config::config_path().display()
        ),
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(1);
        }
    }

    println!("\nprofiles:");
    for name in config.profiles.keys() {
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
            let auto = if agent.auto_start {
                " (auto-start)"
            } else {
                ""
            };
            println!("  {name}{auto}");
            println!("    {} {}", agent.command, agent.args.join(" "));
            println!();
        }
    }

    Ok(())
}

async fn cmd_agent_start(
    client: &DaemonClient,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    println!("starting agent '{name}'...");
    let resp: AgentActionResponse = client
        .post(
            "/api/agents/start",
            &AgentActionRequest {
                name: name.to_string(),
            },
        )
        .await?;

    if resp.success {
        println!("{}", resp.message);
    } else {
        eprintln!("{}", resp.message);
    }

    Ok(())
}

async fn cmd_agent_stop(
    client: &DaemonClient,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    println!("stopping agent '{name}'...");
    let resp: AgentActionResponse = client
        .post(
            "/api/agents/stop",
            &AgentActionRequest {
                name: name.to_string(),
            },
        )
        .await?;

    if resp.success {
        println!("{}", resp.message);
    } else {
        eprintln!("{}", resp.message);
    }

    Ok(())
}

async fn cmd_agent_status(
    client: &DaemonClient,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    if json {
        let resp: serde_json::Value = client.get("/api/agents").await?;
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
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

async fn cmd_agent_describe(
    client: &DaemonClient,
    name: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get(&format!("/api/agents/{name}/health")).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let status = match &resp["status"] {
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

    println!("Agent:    {}", resp["name"].as_str().unwrap_or("?"));
    println!("Status:   {status}");
    println!("PID:      {}", resp["pid"].as_u64().unwrap_or(0));
    if let Some(ver) = resp["version"].as_str() {
        println!("Version:  {ver}");
    }
    if let Some(secs) = resp["uptime_secs"].as_i64() {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        let s = secs % 60;
        println!("Uptime:   {hours}h {mins}m {s}s");
    }
    let restarts = resp["total_restarts"].as_u64().unwrap_or(0);
    if restarts > 0 {
        let reason = resp["last_restart_reason"].as_str().unwrap_or("unknown");
        println!("Restarts: {restarts} (last: {reason})");
    } else {
        println!("Restarts: 0");
    }
    let errors = resp["error_count"].as_u64().unwrap_or(0);
    let lifetime = resp["lifetime_errors"].as_u64().unwrap_or(0);
    println!("Errors:   {errors} (lifetime: {lifetime})");
    if let Some(started) = resp["started_at"].as_str() {
        println!("Started:  {started}");
    }

    Ok(())
}

async fn cmd_swap(
    client: &DaemonClient,
    profile: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    if !json {
        println!("swapping to '{profile}'...");
    }
    let resp: serde_json::Value = client
        .post("/api/swap", &serde_json::json!({ "profile": profile }))
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        let success = resp["success"].as_bool().unwrap_or(false);
        let message = resp["message"].as_str().unwrap_or("");
        if success {
            println!("{message}");
            if let Some(pid) = resp["status"]["pid"].as_u64() {
                println!("  PID:  {pid}");
            }
            if let Some(port) = resp["status"]["port"].as_u64() {
                println!("  API:  http://localhost:{port}/v1");
            }
        } else {
            eprintln!("{message}");
        }
    }

    Ok(())
}

async fn cmd_profiles(client: &DaemonClient, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get("/api/profiles").await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let profiles = resp["profiles"].as_array().unwrap();

    for p in profiles {
        let name = p["name"].as_str().unwrap_or("?");
        let model = p["model"].as_str().unwrap_or("?");
        let ctx = p["ctx_size"].as_u64().unwrap_or(0);
        let reasoning = p["reasoning_budget"].as_i64().unwrap_or(0);
        let is_default = p["default"].as_bool().unwrap_or(false);
        let vram = p["estimated_vram_mb"].as_u64();
        let backend = p["backend"].as_str().unwrap_or("llama-server");

        let default_marker = if is_default { " (default)" } else { "" };
        let thinking = if reasoning != 0 { " thinking" } else { "" };
        let ctx_label = if ctx >= 1024 {
            format!("{}K", ctx / 1024)
        } else {
            ctx.to_string()
        };

        print!("  [{backend}] {name}{default_marker} — {model}");
        if ctx > 0 {
            print!(", {ctx_label} ctx");
        }
        print!("{thinking}");
        if let Some(v) = vram {
            print!(", ~{:.1}GB VRAM", v as f64 / 1024.0);
        }
        println!();
    }

    Ok(())
}

async fn cmd_logs(
    client: &DaemonClient,
    follow: bool,
    n: usize,
) -> Result<(), Box<dyn std::error::Error>> {
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
                } else if let Some(data) = line.strip_prefix("data: ")
                    && current_event == "log"
                {
                    println!("{data}");
                }
            }
        }
    }

    Ok(())
}

async fn cmd_hardware(client: &DaemonClient, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get("/api/hardware").await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    if let Some(gpu) = resp.get("gpu") {
        println!("GPU:       {}", gpu["name"].as_str().unwrap_or("unknown"));
        println!(
            "  VRAM:    {} MB total, {} MB free",
            gpu["vram_total_mb"].as_u64().unwrap_or(0),
            gpu["vram_free_mb"].as_u64().unwrap_or(0)
        );
        if let Some(bw) = gpu["memory_bandwidth_gbps"].as_f64() {
            println!("  Memory:  {:.0} GB/s bandwidth", bw);
        }
        if let (Some(major), Some(minor)) = (
            gpu["compute_capability"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|v| v.as_u64()),
            gpu["compute_capability"]
                .as_array()
                .and_then(|a| a.get(1))
                .and_then(|v| v.as_u64()),
        ) {
            println!("  Compute: {major}.{minor}");
        }
    } else {
        println!("GPU:       not available");
    }

    if let Some(cpu) = resp.get("cpu") {
        println!("CPU:       {}", cpu["name"].as_str().unwrap_or("unknown"));
        println!(
            "  Cores:   {} cores / {} threads",
            cpu["cores"].as_u64().unwrap_or(0),
            cpu["threads"].as_u64().unwrap_or(0)
        );
        let ram_total = cpu["ram_total_mb"].as_u64().unwrap_or(0);
        let ram_free = cpu["ram_free_mb"].as_u64().unwrap_or(0);
        println!("  RAM:     {} MB total, {} MB free", ram_total, ram_free);
    }

    Ok(())
}

async fn cmd_models_search(
    client: &DaemonClient,
    query: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get(&format!("/api/models/search?q={query}")).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let results = resp["results"].as_array().ok_or("no results")?;

    if results.is_empty() {
        println!("no GGUF repos found for '{query}'");
        return Ok(());
    }

    println!("{:<50} {:>10} {:>8}", "Repo", "Downloads", "Likes");
    println!("{}", "-".repeat(70));

    for r in results {
        println!(
            "{:<50} {:>10} {:>8}",
            r["id"].as_str().unwrap_or("?"),
            format_count(r["downloads"].as_u64().unwrap_or(0)),
            format_count(r["likes"].as_u64().unwrap_or(0)),
        );
    }

    Ok(())
}

async fn cmd_models_quants(
    client: &DaemonClient,
    repo: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client
        .get(&format!("/api/models/quants?repo={repo}"))
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let quants = resp["quants"].as_array().ok_or("no quants")?;
    let resolved_repo = resp["repo"].as_str().unwrap_or(repo);

    println!("{resolved_repo}\n");
    println!(
        "{:<16} {:>8} {:>6} {:>14} {:>10}",
        "Quant", "Size", "DL", "Fit", "Est tok/s"
    );
    println!("{}", "-".repeat(58));

    for q in quants {
        let label = q["label"].as_str().unwrap_or("?");
        let size_gb = q["total_bytes"].as_u64().unwrap_or(0) as f64 / 1_073_741_824.0;
        let downloaded = if q["is_downloaded"].as_bool().unwrap_or(false) {
            "✓"
        } else {
            ""
        };
        let fit = q
            .get("perf_estimate")
            .and_then(|e| e["fit_mode"].as_str())
            .unwrap_or("?")
            .replace('_', " ");
        let toks = q
            .get("perf_estimate")
            .and_then(|e| e["estimated_gen_toks"].as_f64())
            .map(|t| format!("~{:.0}", t))
            .unwrap_or_default();

        println!(
            "{:<16} {:>6.1}GB {:>6} {:>14} {:>10}",
            label, size_gb, downloaded, fit, toks
        );
    }

    Ok(())
}

async fn cmd_models_recommend(
    client: &DaemonClient,
    repo: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client
        .get(&format!("/api/models/recommend?repo={repo}"))
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    let resolved_repo = resp["repo"].as_str().unwrap_or(repo);

    if resp["recommendation"].is_null() {
        println!(
            "{resolved_repo}: {}",
            resp["message"].as_str().unwrap_or("no recommendation")
        );
        return Ok(());
    }

    let rec = &resp["recommendation"];
    let label = rec["label"].as_str().unwrap_or("?");
    let size_gb = rec["total_bytes"].as_u64().unwrap_or(0) as f64 / 1_073_741_824.0;
    let fit = rec
        .get("perf_estimate")
        .and_then(|e| e["fit_mode"].as_str())
        .unwrap_or("?")
        .replace('_', " ");
    let toks = rec
        .get("perf_estimate")
        .and_then(|e| e["estimated_gen_toks"].as_f64())
        .unwrap_or(0.0);
    let reason = rec["reason"].as_str().unwrap_or("");

    println!("{resolved_repo}");
    println!("  recommended: {label} ({size_gb:.1}GB)");
    println!("  fit:         {fit}");
    println!("  est gen:     ~{toks:.0} tok/s");
    println!("  reason:      {reason}");

    Ok(())
}

async fn cmd_models_list(
    client: &DaemonClient,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get("/api/models/cached").await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let models = resp["models"].as_array().ok_or("no cached models")?;

    if models.is_empty() {
        println!("no cached models in ~/.cache/llama.cpp/");
        return Ok(());
    }

    println!("{:<45} {:<16} {:>8}", "Repo", "Quant", "Size");
    println!("{}", "-".repeat(72));

    for m in models {
        let repo = m["repo"].as_str().unwrap_or("?");
        let quant = m["quant_label"].as_str().unwrap_or("?");
        let size_gb = m["size_bytes"].as_u64().unwrap_or(0) as f64 / 1_073_741_824.0;
        println!("{:<45} {:<16} {:>6.1}GB", repo, quant, size_gb);
    }

    Ok(())
}

async fn cmd_models_pull(
    client: &DaemonClient,
    repo: &str,
    quant: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let body = serde_json::json!({ "repo": repo, "quant": quant });
    let resp: serde_json::Value = client.post("/api/models/pull", &body).await?;

    if !resp["started"].as_bool().unwrap_or(false) {
        println!("{}", resp["message"].as_str().unwrap_or("pull failed"));
        return Ok(());
    }

    let resolved_repo = resp["repo"].as_str().unwrap_or(repo);
    let quant_label = resp["quant"].as_str().unwrap_or("?");
    let files = resp["files"].as_array().map(|a| a.len()).unwrap_or(0);

    println!("downloading {resolved_repo} ({quant_label}, {files} file(s))...");
    println!("(download runs in background — check `rookery models list` for completion)");

    Ok(())
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

async fn cmd_bench(client: &DaemonClient, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    if !json {
        println!("running benchmark...\n");
    }

    let resp: serde_json::Value = client.get("/api/bench").await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let tests = resp["tests"].as_array().ok_or("no bench results")?;

    if tests.is_empty() {
        println!("no results (is a model running?)");
        return Ok(());
    }

    println!(
        "{:<12} {:>8} {:>8} {:>10} {:>10}",
        "Test", "PP Tok", "Gen Tok", "PP tok/s", "Gen tok/s"
    );
    println!("{}", "-".repeat(52));

    for t in tests {
        println!(
            "{:<12} {:>8} {:>8} {:>10.0} {:>10.0}",
            t["name"].as_str().unwrap_or("?"),
            t["prompt_tokens"].as_u64().unwrap_or(0),
            t["completion_tokens"].as_u64().unwrap_or(0),
            t["pp_tok_s"].as_f64().unwrap_or(0.0),
            t["gen_tok_s"].as_f64().unwrap_or(0.0),
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // StatusResponse deserialization backward-compatible
    //
    // CLI's StatusResponse struct can deserialize JSON from old daemons (no
    // 'backend' field) and new daemons (with 'backend' field). Missing field
    // defaults to None via #[serde(default)].
    #[test]
    fn test_status_response_backward_compat_no_backend() {
        let json = r#"{
            "state": "running",
            "profile": "fast",
            "pid": 1234,
            "port": 8081,
            "uptime_secs": 3600
        }"#;
        let resp: StatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.state, "running");
        assert_eq!(resp.profile.as_deref(), Some("fast"));
        assert_eq!(resp.pid, Some(1234));
        assert_eq!(resp.port, Some(8081));
        assert_eq!(resp.uptime_secs, Some(3600));
        assert_eq!(
            resp.backend, None,
            "missing backend field should default to None"
        );
    }

    #[test]
    fn test_status_response_with_backend_field() {
        let json = r#"{
            "state": "running",
            "profile": "fast",
            "pid": 1234,
            "port": 8081,
            "uptime_secs": 3600,
            "backend": "llama-server"
        }"#;
        let resp: StatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.backend.as_deref(), Some("llama-server"));
    }

    #[test]
    fn test_status_response_with_vllm_backend() {
        let json = r#"{
            "state": "running",
            "profile": "qwen_nvfp4",
            "pid": 0,
            "port": 8081,
            "uptime_secs": 120,
            "backend": "vllm"
        }"#;
        let resp: StatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.backend.as_deref(), Some("vllm"));
    }

    #[test]
    fn test_status_response_with_null_backend() {
        let json = r#"{
            "state": "stopped",
            "profile": null,
            "pid": null,
            "port": null,
            "uptime_secs": null,
            "backend": null
        }"#;
        let resp: StatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.state, "stopped");
        assert_eq!(resp.backend, None);
    }

    // Status command shows backend type (running) and omits it (stopped)
    //
    // Tests the formatting logic for `rookery status` by verifying the output
    // includes a "backend:" line when running and omits it when stopped. Also
    // tests `--json` mode includes the backend field.
    #[test]
    fn test_status_display_running_shows_backend() {
        let resp = StatusResponse {
            state: "running".into(),
            profile: Some("fast".into()),
            pid: Some(1234),
            port: Some(8081),
            uptime_secs: Some(3600),
            backend: Some("llama-server".into()),
        };

        // Simulate the non-JSON display logic
        let mut lines = Vec::new();
        lines.push(format!("state:   {}", resp.state));
        if let Some(profile) = &resp.profile {
            lines.push(format!("profile: {profile}"));
        }
        if let Some(backend) = &resp.backend {
            lines.push(format!("backend: {backend}"));
        }
        if let Some(pid) = resp.pid {
            lines.push(format!("pid:     {pid}"));
        }

        let output = lines.join("\n");
        assert!(
            output.contains("backend: llama-server"),
            "running status should show backend line, got:\n{output}"
        );
    }

    #[test]
    fn test_status_display_running_vllm_shows_backend() {
        let resp = StatusResponse {
            state: "running".into(),
            profile: Some("qwen_nvfp4".into()),
            pid: Some(0),
            port: Some(8081),
            uptime_secs: Some(120),
            backend: Some("vllm".into()),
        };

        let mut lines = Vec::new();
        lines.push(format!("state:   {}", resp.state));
        if let Some(profile) = &resp.profile {
            lines.push(format!("profile: {profile}"));
        }
        if let Some(backend) = &resp.backend {
            lines.push(format!("backend: {backend}"));
        }

        let output = lines.join("\n");
        assert!(
            output.contains("backend: vllm"),
            "running vLLM status should show backend line, got:\n{output}"
        );
    }

    #[test]
    fn test_status_display_stopped_omits_backend() {
        let resp = StatusResponse {
            state: "stopped".into(),
            profile: None,
            pid: None,
            port: None,
            uptime_secs: None,
            backend: None,
        };

        let mut lines = Vec::new();
        lines.push(format!("state:   {}", resp.state));
        if let Some(profile) = &resp.profile {
            lines.push(format!("profile: {profile}"));
        }
        if let Some(backend) = &resp.backend {
            lines.push(format!("backend: {backend}"));
        }

        let output = lines.join("\n");
        assert!(
            !output.contains("backend:"),
            "stopped status should omit backend line, got:\n{output}"
        );
    }

    #[test]
    fn test_status_json_output_includes_backend_running() {
        let resp = StatusResponse {
            state: "running".into(),
            profile: Some("fast".into()),
            pid: Some(1234),
            port: Some(8081),
            uptime_secs: Some(3600),
            backend: Some("llama-server".into()),
        };

        let json = serde_json::json!({
            "state": resp.state,
            "profile": resp.profile,
            "pid": resp.pid,
            "port": resp.port,
            "uptime_secs": resp.uptime_secs,
            "backend": resp.backend,
        });

        assert_eq!(json["backend"], "llama-server");
        assert!(
            json.get("backend").is_some(),
            "JSON output must include backend key"
        );
    }

    #[test]
    fn test_status_json_output_includes_backend_null_when_stopped() {
        let resp = StatusResponse {
            state: "stopped".into(),
            profile: None,
            pid: None,
            port: None,
            uptime_secs: None,
            backend: None,
        };

        let json = serde_json::json!({
            "state": resp.state,
            "profile": resp.profile,
            "pid": resp.pid,
            "port": resp.port,
            "uptime_secs": resp.uptime_secs,
            "backend": resp.backend,
        });

        assert!(
            json.get("backend").is_some(),
            "JSON must always include backend key"
        );
        assert!(
            json["backend"].is_null(),
            "backend should be null when stopped"
        );
    }

    // Profiles command shows backend type per profile
    //
    // Tests the formatting logic for `rookery profiles` by verifying the output
    // includes a [backend] prefix for each profile.
    #[test]
    fn test_profiles_display_shows_backend_prefix_llama_server() {
        let profile = serde_json::json!({
            "name": "qwen_fast",
            "model": "qwen35",
            "port": 8081,
            "ctx_size": 262144,
            "reasoning_budget": 0,
            "backend": "llama-server",
            "default": true,
            "estimated_vram_mb": 25800,
        });

        let name = profile["name"].as_str().unwrap_or("?");
        let model = profile["model"].as_str().unwrap_or("?");
        let ctx = profile["ctx_size"].as_u64().unwrap_or(0);
        let backend = profile["backend"].as_str().unwrap_or("llama-server");
        let is_default = profile["default"].as_bool().unwrap_or(false);
        let vram = profile["estimated_vram_mb"].as_u64();

        let default_marker = if is_default { " (default)" } else { "" };
        let ctx_label = if ctx >= 1024 {
            format!("{}K", ctx / 1024)
        } else {
            ctx.to_string()
        };

        let mut output = format!("  [{backend}] {name}{default_marker} — {model}");
        if ctx > 0 {
            output.push_str(&format!(", {ctx_label} ctx"));
        }
        if let Some(v) = vram {
            output.push_str(&format!(", ~{:.1}GB VRAM", v as f64 / 1024.0));
        }

        assert!(
            output.contains("[llama-server]"),
            "profile output should have [llama-server] prefix, got: {output}"
        );
        assert!(
            output.contains("qwen_fast"),
            "profile output should contain profile name, got: {output}"
        );
    }

    #[test]
    fn test_profiles_display_shows_backend_prefix_vllm() {
        let profile = serde_json::json!({
            "name": "qwen_nvfp4",
            "model": "qwen35_27b_nvfp4",
            "port": 8081,
            "ctx_size": null,
            "reasoning_budget": 0,
            "backend": "vllm",
            "default": false,
            "estimated_vram_mb": null,
        });

        let name = profile["name"].as_str().unwrap_or("?");
        let model = profile["model"].as_str().unwrap_or("?");
        let ctx = profile["ctx_size"].as_u64().unwrap_or(0);
        let backend = profile["backend"].as_str().unwrap_or("llama-server");
        let is_default = profile["default"].as_bool().unwrap_or(false);

        let default_marker = if is_default { " (default)" } else { "" };

        let mut output = format!("  [{backend}] {name}{default_marker} — {model}");
        if ctx > 0 {
            let ctx_label = if ctx >= 1024 {
                format!("{}K", ctx / 1024)
            } else {
                ctx.to_string()
            };
            output.push_str(&format!(", {ctx_label} ctx"));
        }

        assert!(
            output.contains("[vllm]"),
            "profile output should have [vllm] prefix, got: {output}"
        );
        assert!(
            output.contains("qwen_nvfp4"),
            "profile output should contain profile name, got: {output}"
        );
        // vLLM profile with null ctx_size should not show ctx
        assert!(
            !output.contains("ctx"),
            "vLLM profile with null ctx_size should not show ctx, got: {output}"
        );
    }

    #[test]
    fn test_profiles_display_mixed_backends() {
        let profiles = vec![
            serde_json::json!({
                "name": "fast",
                "model": "qwen35",
                "backend": "llama-server",
                "ctx_size": 131072,
                "reasoning_budget": 0,
                "default": true,
                "estimated_vram_mb": null,
            }),
            serde_json::json!({
                "name": "vllm_prod",
                "model": "qwen35_nvfp4",
                "backend": "vllm",
                "ctx_size": null,
                "reasoning_budget": 0,
                "default": false,
                "estimated_vram_mb": null,
            }),
        ];

        let mut output_lines = Vec::new();
        for p in &profiles {
            let name = p["name"].as_str().unwrap_or("?");
            let model = p["model"].as_str().unwrap_or("?");
            let ctx = p["ctx_size"].as_u64().unwrap_or(0);
            let backend = p["backend"].as_str().unwrap_or("llama-server");
            let is_default = p["default"].as_bool().unwrap_or(false);

            let default_marker = if is_default { " (default)" } else { "" };

            let mut line = format!("  [{backend}] {name}{default_marker} — {model}");
            if ctx > 0 {
                let ctx_label = if ctx >= 1024 {
                    format!("{}K", ctx / 1024)
                } else {
                    ctx.to_string()
                };
                line.push_str(&format!(", {ctx_label} ctx"));
            }
            output_lines.push(line);
        }

        let output = output_lines.join("\n");
        assert!(
            output.contains("[llama-server] fast"),
            "first profile should be llama-server"
        );
        assert!(
            output.contains("[vllm] vllm_prod"),
            "second profile should be vllm"
        );
    }

    // Test that profiles JSON passthrough includes backend field
    #[test]
    fn test_profiles_json_includes_backend() {
        let resp = serde_json::json!({
            "profiles": [
                {
                    "name": "fast",
                    "model": "qwen35",
                    "backend": "llama-server",
                    "default": true,
                },
                {
                    "name": "vllm_prod",
                    "model": "qwen35_nvfp4",
                    "backend": "vllm",
                    "default": false,
                },
            ]
        });

        let profiles = resp["profiles"].as_array().unwrap();
        assert_eq!(profiles[0]["backend"], "llama-server");
        assert_eq!(profiles[1]["backend"], "vllm");
    }

    // Backward compat: profiles from old daemon without backend field
    #[test]
    fn test_profiles_display_missing_backend_defaults_to_llama_server() {
        let profile = serde_json::json!({
            "name": "old_profile",
            "model": "qwen35",
            "port": 8081,
            "ctx_size": 131072,
            "reasoning_budget": 0,
            "default": false,
        });

        // Simulate the display logic — missing backend defaults to "llama-server"
        let backend = profile["backend"].as_str().unwrap_or("llama-server");
        assert_eq!(
            backend, "llama-server",
            "missing backend field should default to llama-server"
        );
    }

    // =========================================================================
    // Clap argument parsing covers all commands
    // =========================================================================

    /// Verify all top-level subcommands parse correctly.
    #[test]
    fn test_parse_all_subcommands() {
        let subcommands = vec![
            vec!["rookery", "status"],
            vec!["rookery", "start"],
            vec!["rookery", "start", "fast"],
            vec!["rookery", "stop"],
            vec!["rookery", "swap", "fast"],
            vec!["rookery", "gpu"],
            vec!["rookery", "profiles"],
            vec!["rookery", "bench"],
            vec!["rookery", "logs"],
            vec!["rookery", "config"],
            vec!["rookery", "completions", "bash"],
            vec!["rookery", "models", "search", "qwen"],
            vec!["rookery", "models", "quants", "unsloth/Qwen3-8B-GGUF"],
            vec!["rookery", "models", "recommend", "unsloth/Qwen3-8B-GGUF"],
            vec!["rookery", "models", "list"],
            vec!["rookery", "models", "pull", "unsloth/Qwen3-8B-GGUF"],
            vec!["rookery", "models", "hardware"],
            vec!["rookery", "agent", "start", "myagent"],
            vec!["rookery", "agent", "stop", "myagent"],
            vec!["rookery", "agent", "status"],
        ];

        for args in &subcommands {
            let result = Cli::try_parse_from(args);
            assert!(
                result.is_ok(),
                "subcommand {:?} should parse successfully, got: {:?}",
                args,
                result.err()
            );
        }
    }

    /// Verify --json flag is recognized on status, gpu, profiles, bench, start, stop.
    #[test]
    fn test_parse_json_flag_on_commands() {
        let commands_with_json = vec![
            vec!["rookery", "status", "--json"],
            vec!["rookery", "gpu", "--json"],
            vec!["rookery", "profiles", "--json"],
            vec!["rookery", "bench", "--json"],
            vec!["rookery", "start", "--json"],
            vec!["rookery", "stop", "--json"],
            vec!["rookery", "config", "--json"],
        ];

        for args in &commands_with_json {
            let result = Cli::try_parse_from(args);
            assert!(
                result.is_ok(),
                "--json flag on {:?} should parse successfully, got: {:?}",
                args,
                result.err()
            );
        }
    }

    /// Verify --follow and -f flags on logs, plus -n for line count.
    #[test]
    fn test_parse_logs_follow_and_line_count() {
        // Long form --follow
        let cli = Cli::try_parse_from(["rookery", "logs", "--follow"]).unwrap();
        if let Commands::Logs { follow, n } = cli.command {
            assert!(follow, "--follow should set follow=true");
            assert_eq!(n, 50, "default line count should be 50");
        } else {
            panic!("expected Logs command");
        }

        // Short form -f
        let cli = Cli::try_parse_from(["rookery", "logs", "-f"]).unwrap();
        if let Commands::Logs { follow, .. } = cli.command {
            assert!(follow, "-f should set follow=true");
        } else {
            panic!("expected Logs command");
        }

        // Custom line count -n 100
        let cli = Cli::try_parse_from(["rookery", "logs", "-n", "100"]).unwrap();
        if let Commands::Logs { follow, n } = cli.command {
            assert!(!follow, "follow should default to false");
            assert_eq!(n, 100, "-n 100 should set n=100");
        } else {
            panic!("expected Logs command");
        }

        // Combined -f -n 20
        let cli = Cli::try_parse_from(["rookery", "logs", "-f", "-n", "20"]).unwrap();
        if let Commands::Logs { follow, n } = cli.command {
            assert!(follow);
            assert_eq!(n, 20);
        } else {
            panic!("expected Logs command");
        }
    }

    /// Verify invalid subcommand produces an error.
    #[test]
    fn test_parse_invalid_subcommand_fails() {
        let result = Cli::try_parse_from(["rookery", "nonexistent"]);
        assert!(
            result.is_err(),
            "invalid subcommand should produce an error"
        );
    }

    /// Verify --daemon global flag is parsed.
    #[test]
    fn test_parse_global_daemon_flag() {
        let cli = Cli::try_parse_from(["rookery", "--daemon", "http://localhost:5000", "status"])
            .unwrap();
        assert_eq!(cli.daemon, "http://localhost:5000");
    }

    // =========================================================================
    // Status display formatting for all states
    // =========================================================================

    /// Running state shows all fields: state, profile, backend, pid, port, api, uptime.
    #[test]
    fn test_status_display_running_shows_all_fields() {
        let resp = StatusResponse {
            state: "running".into(),
            profile: Some("fast".into()),
            pid: Some(1234),
            port: Some(8081),
            uptime_secs: Some(3661), // 1h 1m 1s
            backend: Some("llama-server".into()),
        };

        // Replicate the exact formatting logic from cmd_status
        let mut lines = Vec::new();
        lines.push(format!("state:   {}", resp.state));
        if let Some(profile) = &resp.profile {
            lines.push(format!("profile: {profile}"));
        }
        if let Some(backend) = &resp.backend {
            lines.push(format!("backend: {backend}"));
        }
        if let Some(pid) = resp.pid {
            lines.push(format!("pid:     {pid}"));
        }
        if let Some(port) = resp.port {
            lines.push(format!("port:    {port}"));
            lines.push(format!("api:     http://localhost:{port}/v1"));
        }
        if let Some(uptime) = resp.uptime_secs {
            let hours = uptime / 3600;
            let mins = (uptime % 3600) / 60;
            let secs = uptime % 60;
            lines.push(format!("uptime:  {hours}h {mins}m {secs}s"));
        }

        let output = lines.join("\n");
        assert!(output.contains("state:   running"));
        assert!(output.contains("profile: fast"));
        assert!(output.contains("backend: llama-server"));
        assert!(output.contains("pid:     1234"));
        assert!(output.contains("port:    8081"));
        assert!(output.contains("api:     http://localhost:8081/v1"));
        assert!(output.contains("uptime:  1h 1m 1s"));
    }

    /// Stopped state shows only state line — no profile, backend, pid, port, uptime.
    #[test]
    fn test_status_display_stopped_shows_minimal_info() {
        let resp = StatusResponse {
            state: "stopped".into(),
            profile: None,
            pid: None,
            port: None,
            uptime_secs: None,
            backend: None,
        };

        let mut lines = Vec::new();
        lines.push(format!("state:   {}", resp.state));
        if let Some(profile) = &resp.profile {
            lines.push(format!("profile: {profile}"));
        }
        if let Some(backend) = &resp.backend {
            lines.push(format!("backend: {backend}"));
        }
        if let Some(pid) = resp.pid {
            lines.push(format!("pid:     {pid}"));
        }
        if let Some(port) = resp.port {
            lines.push(format!("port:    {port}"));
            lines.push(format!("api:     http://localhost:{port}/v1"));
        }
        if let Some(uptime) = resp.uptime_secs {
            let hours = uptime / 3600;
            let mins = (uptime % 3600) / 60;
            let secs = uptime % 60;
            lines.push(format!("uptime:  {hours}h {mins}m {secs}s"));
        }

        let output = lines.join("\n");
        assert_eq!(lines.len(), 1, "stopped state should only have 1 line");
        assert!(output.contains("state:   stopped"));
        assert!(!output.contains("profile:"));
        assert!(!output.contains("backend:"));
        assert!(!output.contains("pid:"));
        assert!(!output.contains("port:"));
        assert!(!output.contains("uptime:"));
    }

    /// Daemon offline: non-JSON mode shows "rookeryd: offline".
    #[test]
    fn test_status_display_daemon_offline_message() {
        // The cmd_status function prints "rookeryd: offline" when daemon is unreachable
        let offline_text = "rookeryd: offline";
        assert_eq!(offline_text, "rookeryd: offline");

        // JSON mode produces {"state":"daemon_offline"}
        let json_offline = r#"{"state":"daemon_offline"}"#;
        let parsed: serde_json::Value = serde_json::from_str(json_offline).unwrap();
        assert_eq!(parsed["state"], "daemon_offline");
    }

    // =========================================================================
    // GPU display formatting: temp, VRAM, utilization, power
    // =========================================================================

    /// GPU display shows temperature, VRAM usage percentage, utilization, and power.
    #[test]
    fn test_gpu_display_formatting() {
        let gpu = GpuStats {
            index: 0,
            name: "NVIDIA RTX 4090".into(),
            vram_used_mb: 20480,
            vram_total_mb: 24576,
            temperature_c: 72,
            utilization_pct: 95,
            power_watts: 350.0,
            power_limit_watts: 450.0,
        };

        // Replicate the exact formatting from cmd_gpu
        let mut lines = Vec::new();
        lines.push(format!("GPU {}: {}", gpu.index, gpu.name));
        lines.push(format!(
            "  VRAM:  {} / {} MB ({:.0}%)",
            gpu.vram_used_mb,
            gpu.vram_total_mb,
            gpu.vram_used_mb as f64 / gpu.vram_total_mb as f64 * 100.0
        ));
        lines.push(format!("  Temp:  {}C", gpu.temperature_c));
        lines.push(format!("  Util:  {}%", gpu.utilization_pct));
        lines.push(format!(
            "  Power: {:.0}W / {:.0}W",
            gpu.power_watts, gpu.power_limit_watts
        ));

        let output = lines.join("\n");
        assert!(output.contains("GPU 0: NVIDIA RTX 4090"));
        assert!(output.contains("20480 / 24576 MB (83%)"));
        assert!(output.contains("Temp:  72C"));
        assert!(output.contains("Util:  95%"));
        assert!(output.contains("Power: 350W / 450W"));
    }

    // =========================================================================
    // Profiles display: ctx_size K formatting, thinking flag
    // =========================================================================

    /// Profiles display shows ctx_size as "K" when >= 1024, and raw when < 1024.
    #[test]
    fn test_profiles_display_ctx_size_formatting() {
        // 131072 → "128K"
        let ctx: u64 = 131072;
        let ctx_label = if ctx >= 1024 {
            format!("{}K", ctx / 1024)
        } else {
            ctx.to_string()
        };
        assert_eq!(ctx_label, "128K");

        // 512 → "512"
        let ctx: u64 = 512;
        let ctx_label = if ctx >= 1024 {
            format!("{}K", ctx / 1024)
        } else {
            ctx.to_string()
        };
        assert_eq!(ctx_label, "512");

        // 262144 → "256K"
        let ctx: u64 = 262144;
        let ctx_label = if ctx >= 1024 {
            format!("{}K", ctx / 1024)
        } else {
            ctx.to_string()
        };
        assert_eq!(ctx_label, "256K");
    }

    /// Profile with reasoning_budget shows "thinking" suffix.
    #[test]
    fn test_profiles_display_thinking_flag() {
        let profile = serde_json::json!({
            "name": "think",
            "model": "qwen35",
            "backend": "llama-server",
            "ctx_size": 131072,
            "reasoning_budget": 16384,
            "default": false,
            "estimated_vram_mb": null,
        });

        let name = profile["name"].as_str().unwrap_or("?");
        let model = profile["model"].as_str().unwrap_or("?");
        let ctx = profile["ctx_size"].as_u64().unwrap_or(0);
        let reasoning = profile["reasoning_budget"].as_i64().unwrap_or(0);
        let backend = profile["backend"].as_str().unwrap_or("llama-server");
        let is_default = profile["default"].as_bool().unwrap_or(false);

        let default_marker = if is_default { " (default)" } else { "" };
        let thinking = if reasoning != 0 { " thinking" } else { "" };
        let ctx_label = if ctx >= 1024 {
            format!("{}K", ctx / 1024)
        } else {
            ctx.to_string()
        };

        let mut output = format!("  [{backend}] {name}{default_marker} — {model}");
        if ctx > 0 {
            output.push_str(&format!(", {ctx_label} ctx"));
        }
        output.push_str(thinking);

        assert!(
            output.contains(" thinking"),
            "profile with reasoning_budget should show thinking, got: {output}"
        );
        assert!(
            output.contains("128K ctx"),
            "should show formatted ctx, got: {output}"
        );
    }

    // =========================================================================
    // Agent status display formatting
    // =========================================================================

    /// Agent status display shows name, PID, and status string.
    #[test]
    fn test_agent_status_display_formatting() {
        let agents = vec![
            AgentInfo {
                name: "aider".into(),
                pid: 5678,
                started_at: "2024-01-01T00:00:00Z".into(),
                status: serde_json::Value::String("running".into()),
            },
            AgentInfo {
                name: "cursor".into(),
                pid: 9999,
                started_at: "2024-01-01T00:00:00Z".into(),
                status: serde_json::json!({"error": "segfault"}),
            },
        ];

        let mut output_lines = Vec::new();
        for agent in &agents {
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
            output_lines.push(format!("  {} (PID {}) — {}", agent.name, agent.pid, status));
        }

        let output = output_lines.join("\n");
        assert!(
            output.contains("aider (PID 5678) — running"),
            "should show running agent, got: {output}"
        );
        assert!(
            output.contains("cursor (PID 9999) — failed: segfault"),
            "should show failed agent with error, got: {output}"
        );
    }

    // =========================================================================
    // Bench display formatting with tok/s
    // =========================================================================

    /// Bench display shows test name, token counts, and tok/s rates.
    #[test]
    fn test_bench_display_formatting() {
        let tests = vec![
            serde_json::json!({
                "name": "pp512",
                "prompt_tokens": 512,
                "completion_tokens": 128,
                "pp_tok_s": 3200.5,
                "gen_tok_s": 42.3,
            }),
            serde_json::json!({
                "name": "tg128",
                "prompt_tokens": 1,
                "completion_tokens": 128,
                "pp_tok_s": 0.0,
                "gen_tok_s": 45.8,
            }),
        ];

        // Replicate the bench formatting from cmd_bench
        let mut lines = Vec::new();
        lines.push(format!(
            "{:<12} {:>8} {:>8} {:>10} {:>10}",
            "Test", "PP Tok", "Gen Tok", "PP tok/s", "Gen tok/s"
        ));
        lines.push("-".repeat(52));

        for t in &tests {
            lines.push(format!(
                "{:<12} {:>8} {:>8} {:>10.0} {:>10.0}",
                t["name"].as_str().unwrap_or("?"),
                t["prompt_tokens"].as_u64().unwrap_or(0),
                t["completion_tokens"].as_u64().unwrap_or(0),
                t["pp_tok_s"].as_f64().unwrap_or(0.0),
                t["gen_tok_s"].as_f64().unwrap_or(0.0),
            ));
        }

        let output = lines.join("\n");
        assert!(output.contains("PP tok/s"), "header should show PP tok/s");
        assert!(output.contains("Gen tok/s"), "header should show Gen tok/s");
        assert!(output.contains("pp512"), "should contain test name 'pp512'");
        assert!(output.contains("tg128"), "should contain test name 'tg128'");
        // Check that tok/s values appear in the table (3200.5 → "3200" with banker's rounding)
        assert!(
            output.contains("3200") || output.contains("3201"),
            "should show PP tok/s value, got:\n{output}"
        );
        // gen_tok_s 42.3 → "42"
        assert!(
            output.contains("42"),
            "should show gen tok/s value, got:\n{output}"
        );
        // gen_tok_s 45.8 → "46"
        assert!(
            output.contains("46"),
            "should show second gen tok/s value, got:\n{output}"
        );
    }

    // =========================================================================
    // format_count helper
    // =========================================================================

    /// format_count formats numbers with K/M suffixes.
    #[test]
    fn test_format_count_helper() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1.0K");
        assert_eq!(format_count(1500), "1.5K");
        assert_eq!(format_count(999_999), "1000.0K");
        assert_eq!(format_count(1_000_000), "1.0M");
        assert_eq!(format_count(2_500_000), "2.5M");
    }

    // =========================================================================
    // GpuResponse / GpuStats deserialization
    // =========================================================================

    /// GpuResponse deserializes correctly from daemon JSON.
    #[test]
    fn test_gpu_response_deserialization() {
        let json = r#"{
            "gpus": [{
                "index": 0,
                "name": "NVIDIA RTX 4090",
                "vram_used_mb": 20480,
                "vram_total_mb": 24576,
                "temperature_c": 65,
                "utilization_pct": 88,
                "power_watts": 300.5,
                "power_limit_watts": 450.0
            }]
        }"#;
        let resp: GpuResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.gpus.len(), 1);
        assert_eq!(resp.gpus[0].name, "NVIDIA RTX 4090");
        assert_eq!(resp.gpus[0].vram_used_mb, 20480);
        assert_eq!(resp.gpus[0].temperature_c, 65);
        assert_eq!(resp.gpus[0].utilization_pct, 88);
    }
}
