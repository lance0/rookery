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

// ── 4. The `--json` error contract (LAN-1102) ───────────────────────────
//
// Two halves of one contract: `--json` must not exit 0 on a failure the plain
// mode exits 1 on, and must not leave `| jq` with empty stdin.

fn parse_stdout(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| {
        panic!(
            "stdout must be parseable JSON in --json mode ({e}); stdout was {:?}",
            stdout(out)
        )
    })
}

/// The headline bug: plain mode exits 1, `--json` exited 0 on the same body.
#[test]
fn start_json_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(FAILED);
    let out = run(port, &["start", "fast", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "`rookery start --json` must agree with plain `rookery start`, which \
         exits 1 on the identical body — the machine-readable mode was the one \
         that lied. stdout: {}",
        stdout(&out)
    );
    assert_eq!(
        parse_stdout(&out)["message"],
        "server failed to start",
        "the daemon body must still be printed verbatim, not replaced"
    );
}

#[test]
fn swap_json_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(FAILED);
    let out = run(port, &["swap", "fast", "--json"]);
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    assert_eq!(parse_stdout(&out)["success"], false);
}

#[test]
fn agent_update_json_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(r#"{"success":false,"message":"update failed","version":"0.20.0"}"#);
    let out = run(port, &["agent", "update", "hermes", "--json"]);
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    assert_eq!(parse_stdout(&out)["version"], "0.20.0");
}

/// The other side of the same coin — success must stay exit 0 with the body
/// untouched, or every `rookery start --json | jq` in a script breaks.
#[test]
fn start_json_still_exits_zero_on_success() {
    let port = stub_daemon(r#"{"success":true,"message":"server started","status":{"pid":42}}"#);
    let out = run(port, &["start", "fast", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the JSON success path must not regress into exit 1; stderr: {}",
        stderr(&out)
    );
    let body = parse_stdout(&out);
    assert_eq!(body["success"], true);
    assert!(
        body.get("error").is_none(),
        "the success body must be the daemon's, unwrapped: {body}"
    );
}

/// `rookery gpu --json | jq .` used to hand jq an empty stdin whenever the
/// daemon was down.
#[test]
fn json_client_error_prints_an_envelope_instead_of_empty_stdout() {
    let out = run_against(CLOSED, &["gpu", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let body = parse_stdout(&out);
    assert!(
        body["error"].as_str().unwrap_or_default().contains(CLOSED),
        "the envelope must name the probed URL: {body}"
    );
    assert_eq!(body["daemon_url"], CLOSED);
}

/// Every `--json` error path shares one key, whichever command produced it —
/// that is the whole point of the envelope, and `status` (which has its own
/// richer offline body) has to be part of it rather than a second shape.
#[test]
fn every_json_error_path_carries_the_shared_error_key() {
    for args in [
        vec!["gpu", "--json"],
        vec!["profiles", "--json"],
        vec!["status", "--json"],
        vec!["bench", "--json"],
        vec!["agent", "status", "--json"],
        vec!["models", "list", "--json"],
        vec!["releases", "--json"],
    ] {
        let out = run_against(CLOSED, &args);
        assert_eq!(out.status.code(), Some(1), "for `{}`", args.join(" "));
        let body = parse_stdout(&out);
        assert!(
            body["error"].is_string(),
            "`rookery {} ` must emit a string .error: {body}",
            args.join(" ")
        );
        assert_eq!(body["daemon_url"], CLOSED, "for `{}`", args.join(" "));
    }
}

/// LAN-1084 shipped `.state` on this body a wave ago; converging on the shared
/// envelope must add keys, not swap them out from under existing scripts.
#[test]
fn status_json_offline_keeps_its_state_field_while_gaining_error() {
    let out = run_against(CLOSED, &["status", "--json"]);
    let body = parse_stdout(&out);
    assert_eq!(body["state"], "daemon_offline");
    assert!(body["error"].is_string(), "{body}");
}

/// Usage errors are clap's, and clap exits 2. That is why daemon-unreachable
/// and daemon-reported-failure both stay 1: taking 2 for either would collide
/// with "you typed the command wrong", the one distinction a script most needs.
#[test]
fn usage_errors_exit_2_so_runtime_failures_can_own_1() {
    let out = run_against(CLOSED, &["bogus-subcommand"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));

    let out = run_against(CLOSED, &["swap"]); // missing required <profile>
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
}

/// The other five failure paths use stderr; this one printed to stdout, so a
/// failed pull landed in whatever file the caller was capturing the report to.
#[test]
fn models_pull_failure_goes_to_stderr_not_stdout() {
    let port = stub_daemon(r#"{"started":false,"message":"no GGUF files found"}"#);
    let out = run(port, &["models", "pull", "unsloth/Qwen3.8-27B-GGUF"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no GGUF files found"),
        "stderr: {}",
        stderr(&out)
    );
    assert!(
        !stdout(&out).contains("no GGUF files found"),
        "stdout: {}",
        stdout(&out)
    );
}

// ── 5. `sleep` / `wake` join the contract (LAN-1148) ────────────────────
//
// Both printed `message` and returned Ok(()), so both exited 0 in plain *and*
// `--json` mode on a daemon-reported failure. `wake` is the dangerous one:
// `rookery wake && rookery bench` benched a server that never woke.
//
// The idempotency call: the daemon already draws the line, returning
// success:true for "server already sleeping" / "already running" and false only
// for "server is not running" / "server is not sleeping". Those two are not
// no-ops reaching the desired end state — a Stopped server that you `sleep`
// cannot then be `wake`d — so the CLI honours the field as-is.

const NOT_RUNNING: &str = r#"{"success":false,"message":"server is not running","status":{}}"#;
const NOT_SLEEPING: &str = r#"{"success":false,"message":"server is not sleeping","status":{}}"#;

#[test]
fn sleep_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(NOT_RUNNING);
    let out = run(port, &["sleep"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "`rookery sleep` must exit 1 when the daemon reports success:false; \
         stderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("server is not running"),
        "the failure message must reach stderr, not stdout: {:?}",
        stdout(&out)
    );
}

#[test]
fn wake_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(NOT_SLEEPING);
    let out = run(port, &["wake"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "`rookery wake && rookery bench` otherwise benches a server that never \
         woke — the exact boot-script shape LAN-1084 was filed for. stderr: {}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("server is not sleeping"));
}

/// The daemon also reports `success: server_state.is_running()` after actually
/// attempting the wake, so a wake that started but failed its health check is a
/// success:false body with a cheerful message. The exit code must follow the
/// field, not the wording.
#[test]
fn wake_exits_nonzero_when_the_woken_server_is_not_running() {
    let port = stub_daemon(r#"{"success":false,"message":"server failed to wake","status":{}}"#);
    let out = run(port, &["wake"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
}

#[test]
fn sleep_json_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(NOT_RUNNING);
    let out = run(port, &["sleep", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "`--json` must agree with plain mode on the identical body; stdout: {}",
        stdout(&out)
    );
    assert_eq!(parse_stdout(&out)["message"], "server is not running");
}

#[test]
fn wake_json_exits_nonzero_when_daemon_reports_failure() {
    let port = stub_daemon(NOT_SLEEPING);
    let out = run(port, &["wake", "--json"]);
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    assert_eq!(parse_stdout(&out)["success"], false);
}

/// The compatibility half of the idempotency decision: a shutdown script that
/// calls `rookery sleep` defensively against an already-sleeping server keeps
/// exiting 0, because the daemon calls that case success:true.
#[test]
fn sleep_still_exits_zero_when_already_sleeping() {
    let port = stub_daemon(r#"{"success":true,"message":"server already sleeping","status":{}}"#);
    for args in [vec!["sleep"], vec!["sleep", "--json"]] {
        let out = run(port, &args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "the idempotent repeat-sleep must not become a failure, for `{}`; \
             stderr: {}",
            args.join(" "),
            stderr(&out)
        );
        assert!(stdout(&out).contains("server already sleeping"));
    }
}

#[test]
fn wake_still_exits_zero_on_success() {
    let port = stub_daemon(r#"{"success":true,"message":"server woke with profile 'fast'"}"#);
    for args in [vec!["wake"], vec!["wake", "--json"]] {
        let out = run(port, &args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "for `{}`; stderr: {}",
            args.join(" "),
            stderr(&out)
        );
        assert!(stdout(&out).contains("server woke"));
    }
}
