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
    /// Run a quick benchmark
    Bench,
    /// Validate config file
    #[command(name = "config")]
    ConfigValidate,
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
    },
    /// List available quants for a model repo
    Quants {
        /// Repo (e.g., unsloth/Qwen3-8B-GGUF or just Qwen3-8B)
        repo: String,
    },
    /// Recommend best-fit quant for your hardware
    Recommend {
        /// Repo (e.g., unsloth/Qwen3-8B-GGUF or just Qwen3-8B)
        repo: String,
    },
    /// List locally cached models
    List,
    /// Download a model
    Pull {
        /// Repo (e.g., unsloth/Qwen3-8B-GGUF or just Qwen3-8B)
        repo: String,
        /// Quant to download (auto-picks best fit if omitted)
        #[arg(long)]
        quant: Option<String>,
    },
    /// Show hardware profile
    Hardware,
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
        Commands::Bench => cmd_bench(&client).await,
        Commands::ConfigValidate => cmd_config_validate().await,
        Commands::Agent { cmd } => match cmd {
            AgentCommands::Start { name } => cmd_agent_start(&client, &name).await,
            AgentCommands::Stop { name } => cmd_agent_stop(&client, &name).await,
            AgentCommands::Status => cmd_agent_status(&client).await,
        },
        Commands::Models { cmd } => match cmd {
            ModelCommands::Search { query } => cmd_models_search(&client, &query).await,
            ModelCommands::Quants { repo } => cmd_models_quants(&client, &repo).await,
            ModelCommands::Recommend { repo } => cmd_models_recommend(&client, &repo).await,
            ModelCommands::List => cmd_models_list(&client).await,
            ModelCommands::Pull { repo, quant } => cmd_models_pull(&client, &repo, quant).await,
            ModelCommands::Hardware => cmd_hardware(&client).await,
        },
        Commands::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "rookery",
                &mut std::io::stdout(),
            );
            Ok(())
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

async fn cmd_hardware(client: &DaemonClient) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get("/api/hardware").await?;

    if let Some(gpu) = resp.get("gpu") {
        println!("GPU:       {}", gpu["name"].as_str().unwrap_or("unknown"));
        println!("  VRAM:    {} MB total, {} MB free",
            gpu["vram_total_mb"].as_u64().unwrap_or(0),
            gpu["vram_free_mb"].as_u64().unwrap_or(0));
        if let Some(bw) = gpu["memory_bandwidth_gbps"].as_f64() {
            println!("  Memory:  {:.0} GB/s bandwidth", bw);
        }
        if let (Some(major), Some(minor)) = (
            gpu["compute_capability"].as_array().and_then(|a| a.first()).and_then(|v| v.as_u64()),
            gpu["compute_capability"].as_array().and_then(|a| a.get(1)).and_then(|v| v.as_u64()),
        ) {
            println!("  Compute: {major}.{minor}");
        }
    } else {
        println!("GPU:       not available");
    }

    if let Some(cpu) = resp.get("cpu") {
        println!("CPU:       {}", cpu["name"].as_str().unwrap_or("unknown"));
        println!("  Cores:   {} cores / {} threads",
            cpu["cores"].as_u64().unwrap_or(0),
            cpu["threads"].as_u64().unwrap_or(0));
        let ram_total = cpu["ram_total_mb"].as_u64().unwrap_or(0);
        let ram_free = cpu["ram_free_mb"].as_u64().unwrap_or(0);
        println!("  RAM:     {} MB total, {} MB free",
            ram_total, ram_free);
    }

    Ok(())
}

async fn cmd_models_search(client: &DaemonClient, query: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get(&format!("/api/models/search?q={query}")).await?;
    let results = resp["results"].as_array().ok_or("no results")?;

    if results.is_empty() {
        println!("no GGUF repos found for '{query}'");
        return Ok(());
    }

    println!("{:<50} {:>10} {:>8}", "Repo", "Downloads", "Likes");
    println!("{}", "-".repeat(70));

    for r in results {
        println!("{:<50} {:>10} {:>8}",
            r["id"].as_str().unwrap_or("?"),
            format_count(r["downloads"].as_u64().unwrap_or(0)),
            format_count(r["likes"].as_u64().unwrap_or(0)),
        );
    }

    Ok(())
}

async fn cmd_models_quants(client: &DaemonClient, repo: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get(&format!("/api/models/quants?repo={repo}")).await?;
    let quants = resp["quants"].as_array().ok_or("no quants")?;
    let resolved_repo = resp["repo"].as_str().unwrap_or(repo);

    println!("{resolved_repo}\n");
    println!("{:<16} {:>8} {:>6} {:>14} {:>10}", "Quant", "Size", "DL", "Fit", "Est tok/s");
    println!("{}", "-".repeat(58));

    for q in quants {
        let label = q["label"].as_str().unwrap_or("?");
        let size_gb = q["total_bytes"].as_u64().unwrap_or(0) as f64 / 1_073_741_824.0;
        let downloaded = if q["is_downloaded"].as_bool().unwrap_or(false) { "✓" } else { "" };
        let fit = q.get("perf_estimate")
            .and_then(|e| e["fit_mode"].as_str())
            .unwrap_or("?")
            .replace('_', " ");
        let toks = q.get("perf_estimate")
            .and_then(|e| e["estimated_gen_toks"].as_f64())
            .map(|t| format!("~{:.0}", t))
            .unwrap_or_default();

        println!("{:<16} {:>6.1}GB {:>6} {:>14} {:>10}", label, size_gb, downloaded, fit, toks);
    }

    Ok(())
}

async fn cmd_models_recommend(client: &DaemonClient, repo: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get(&format!("/api/models/recommend?repo={repo}")).await?;
    let resolved_repo = resp["repo"].as_str().unwrap_or(repo);

    if resp["recommendation"].is_null() {
        println!("{resolved_repo}: {}", resp["message"].as_str().unwrap_or("no recommendation"));
        return Ok(());
    }

    let rec = &resp["recommendation"];
    let label = rec["label"].as_str().unwrap_or("?");
    let size_gb = rec["total_bytes"].as_u64().unwrap_or(0) as f64 / 1_073_741_824.0;
    let fit = rec.get("perf_estimate")
        .and_then(|e| e["fit_mode"].as_str())
        .unwrap_or("?")
        .replace('_', " ");
    let toks = rec.get("perf_estimate")
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

async fn cmd_models_list(client: &DaemonClient) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    let resp: serde_json::Value = client.get("/api/models/cached").await?;
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

async fn cmd_models_pull(client: &DaemonClient, repo: &str, quant: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
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

async fn cmd_bench(client: &DaemonClient) -> Result<(), Box<dyn std::error::Error>> {
    if !client.health().await {
        return Err("rookeryd is not running".into());
    }

    println!("running benchmark...\n");

    let resp: serde_json::Value = client.get("/api/bench").await?;
    let tests = resp["tests"].as_array().ok_or("no bench results")?;

    if tests.is_empty() {
        println!("no results (is a model running?)");
        return Ok(());
    }

    println!("{:<12} {:>8} {:>8} {:>10} {:>10}", "Test", "PP Tok", "Gen Tok", "PP tok/s", "Gen tok/s");
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
