//! Exit-code contract for the CLI (LAN-1084).
//!
//! The daemon reports genuine failures as HTTP 200 with `success: false`, so the
//! exit code has to be derived from the response body. These tests drive the real
//! binary against a stub daemon and assert on the process exit status — that is
//! the only place the bug is observable.
//!
//! Stdlib only: a std TcpListener speaking enough HTTP/1.1 to answer reqwest.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};

/// Stub daemon: `/api/health` always 200, every other path answers `body` with 200.
/// Returns the port it listens on.
fn stub_daemon(body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Ok(peek) = stream.try_clone() else {
                continue;
            };
            let mut reader = BufReader::new(peek);

            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();

            // Drain headers, noting the body length so the request is fully read
            // before we answer (otherwise the client can see a reset instead).
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            if content_length > 0 {
                let mut buf = vec![0u8; content_length];
                let _ = reader.read_exact(&mut buf);
            }

            let payload = if path == "/api/health" {
                "{\"status\":\"ok\"}"
            } else {
                body
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.flush();
        }
    });

    port
}

/// Run the real binary against `daemon_url`, isolated from the developer's own
/// config (a nonexistent XDG dir yields the silent "no config" path).
fn run_against(daemon_url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rookery"))
        .env("XDG_CONFIG_HOME", "/nonexistent-rookery-lan1084")
        .arg("--daemon")
        .arg(daemon_url)
        .args(args)
        .output()
        .expect("run rookery")
}

fn run(port: u16, args: &[&str]) -> Output {
    run_against(&format!("http://127.0.0.1:{port}"), args)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ── 1. HTTP 200 + success:false must exit non-zero ──────────────────────

const FAILED: &str = r#"{"success":false,"message":"server failed to start","status":{}}"#;

#[test]
fn start_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(FAILED);
    let out = run(port, &["start", "fast"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "`rookery start` must exit 1 when the daemon reports success:false; \
         a boot script doing `rookery start && rookery agent start hermes` \
         otherwise starts the agent against a dead server. stderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("server failed to start"),
        "failure message must still reach stderr"
    );
}

#[test]
fn swap_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(FAILED);
    let out = run(port, &["swap", "fast"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
}

#[test]
fn agent_start_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(r#"{"success":false,"message":"agent not configured"}"#);
    let out = run(port, &["agent", "start", "hermes"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("agent not configured"));
}

#[test]
fn agent_stop_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(r#"{"success":false,"message":"agent not running"}"#);
    let out = run(port, &["agent", "stop", "hermes"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
}

#[test]
fn agent_update_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(r#"{"success":false,"message":"update failed","version":"0.20.0"}"#);
    let out = run(port, &["agent", "update", "hermes"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("version: 0.20.0"),
        "the version line must still print before exiting; stdout: {}",
        stdout(&out)
    );
}

#[test]
fn models_pull_exits_nonzero_when_pull_does_not_start() {
    let port = stub_daemon(r#"{"started":false,"message":"no GGUF files found"}"#);
    let out = run(port, &["models", "pull", "unsloth/Qwen3.8-27B-GGUF"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
}

#[test]
fn start_still_exits_zero_on_success() {
    let port = stub_daemon(r#"{"success":true,"message":"server started","status":{"pid":42}}"#);
    let out = run(port, &["start", "fast"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the success path must not regress into exit 1; stderr: {}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("server started"));
}

// ── 2/3. Daemon-offline diagnostics ─────────────────────────────────────

/// Port 1 is privileged and unbound: connect() fails immediately.
const CLOSED: &str = "http://127.0.0.1:1";

#[test]
fn offline_error_names_the_probed_url() {
    let out = run_against(CLOSED, &["gpu"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("rookeryd is not running at http://127.0.0.1:1"),
        "the message must name the URL that was probed, or a bad config file \
         looks exactly like a stopped daemon. stderr: {}",
        stderr(&out)
    );
}

#[test]
fn status_exits_nonzero_when_daemon_offline() {
    let out = run_against(CLOSED, &["status"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "`rookery status || alert` can never fire if status always exits 0"
    );
    assert!(stdout(&out).contains("rookeryd: offline"));
    assert!(
        stdout(&out).contains("http://127.0.0.1:1"),
        "stdout: {}",
        stdout(&out)
    );
}

#[test]
fn status_json_exits_nonzero_but_still_prints_parseable_json() {
    let out = run_against(CLOSED, &["status", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("stdout must still be valid JSON for `| jq`");
    assert_eq!(parsed["state"], "daemon_offline");
    assert_eq!(parsed["daemon_url"], "http://127.0.0.1:1");
}

#[test]
fn broken_config_warns_instead_of_silently_falling_back() {
    let dir = std::env::temp_dir().join(format!("rookery-lan1084-{}", std::process::id()));
    let config_dir = dir.join("rookery");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "listen = \n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_rookery"))
        .env("XDG_CONFIG_HOME", &dir)
        .arg("status")
        .output()
        .expect("run rookery");
    let err = stderr(&out);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        err.contains("warning:") && err.contains("config parse error"),
        "a config that exists but fails to parse must not be swallowed; stderr: {err}"
    );
    assert!(
        err.contains("config.toml"),
        "the warning must name the file it failed to read; stderr: {err}"
    );
    assert!(
        err.contains("127.0.0.1:3000"),
        "the warning must name the fallback URL it is about to probe; stderr: {err}"
    );
}

#[test]
fn missing_config_stays_quiet() {
    // The pre-install / remote-client case must not gain a warning on every command.
    let out = run_against(CLOSED, &["status"]);
    assert!(
        !stderr(&out).contains("warning:"),
        "stderr: {}",
        stderr(&out)
    );
}
