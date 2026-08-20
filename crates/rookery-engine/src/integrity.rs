//! Periodic SQLite integrity checking for a managed agent's databases.
//!
//! Runs `PRAGMA quick_check` against every `*.db` under the agent's data
//! directory, read-only, in a subprocess. It reports; it never acts.
//!
//! # `count(*)` is not an integrity signal
//!
//! This is the whole reason the module exists in this shape. When Hermes'
//! `state.db` was corrupted on 2026-08-15, `count(*)` cheerfully returned
//! 25,654 rows while `max(id)` failed outright — the count was answered out of
//! an index without ever touching the damaged leaf pages, so it looked like
//! proof of health and cost hours of debugging. Reproduced here on a
//! deliberately corrupted copy with the roles *reversed*: `max(id)` returned
//! 25000 and a point lookup returned real row data, while `count(*)` raised
//! SQLITE_CORRUPT. Which queries survive depends only on which pages the query
//! planner happens to visit, so a query succeeding is evidence of nothing in
//! either direction. Only a full page traversal — `quick_check` — counts.
//!
//! `quick_check` is used rather than `integrity_check` because it skips the
//! expensive index-versus-table cross-checks and would have caught this the
//! next morning regardless: measured 2.0s on a 392 MB copy of the real file.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::logs::LogBuffer;

/// The checker binary. Not a declared dependency of rookery — a box without it
/// degrades to a warning, never a crash and never a false "healthy".
pub const SQLITE3: &str = "sqlite3";

/// Per-file ceiling. A healthy 392 MB database measures ~2s; this only exists so
/// a pathological file cannot pin the check forever.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);

/// `quick_check` reports up to 100 problems. The first few identify the file and
/// the damaged tree; the rest are noise in a log line.
const MAX_DETAIL_LINES: usize = 5;

/// ponytail: flat cap instead of a config knob. A real agent data directory holds
/// a handful of databases (Hermes has six); this only stops a surprise directory
/// of thousands from turning a daily check into an all-night one.
const MAX_DATABASES: usize = 64;

// SQLite result codes returned by the CLI when it refuses the file outright,
// i.e. it never got far enough to print a quick_check report.
const SQLITE_CORRUPT: i32 = 11;
const SQLITE_NOTADB: i32 = 26;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbCheckOutcome {
    /// `quick_check` traversed the file and found nothing wrong.
    Ok,
    /// Damage found, or SQLite rejected the file as corrupt/not-a-database.
    Corrupt(String),
    /// The check could not be performed. **Not** evidence of health.
    Unchecked(String),
}

/// Every `*.db` in `root` and in its immediate subdirectories, sorted.
///
/// One level down is deliberate: it picks up the nested `cron/executions.db`
/// alongside the top-level `state.db`, `memory_store.db`, `kanban.db` and
/// `verification_evidence.db`, while leaving deeper trees like
/// `state-snapshots/<timestamp>/` alone — those are backups, and walking them
/// would re-check hundreds of megabytes of frozen copies every night.
///
/// `-wal` and `-shm` sidecars are skipped by the extension filter; they are not
/// separately checkable and are covered by checking the database they belong to.
/// `DirEntry::file_type` does not follow symlinks, so a symlinked subdirectory
/// is not descended into and cannot produce a cycle.
pub fn discover_databases(root: &Path) -> Vec<PathBuf> {
    fn dbs_in(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "db")
                && entry.file_type().is_ok_and(|t| t.is_file())
            {
                out.push(path);
            }
        }
    }

    let mut found = Vec::new();
    dbs_in(root, &mut found);

    if let Ok(entries) = std::fs::read_dir(root) {
        let mut subdirs: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        subdirs.sort();
        for dir in subdirs {
            dbs_in(&dir, &mut found);
        }
    }

    found.sort();
    found.dedup();
    found.truncate(MAX_DATABASES);
    found
}

/// Run `PRAGMA quick_check` against one database file.
///
/// # Read-only, and why it has to be
///
/// `-readonly` is the CLI's `SQLITE_OPEN_READONLY`, equivalent to
/// `file:...?mode=ro` but without having to percent-escape a path into a URI.
/// It is not just hygiene against a buggy checker: opening a WAL database
/// *read-write* and closing it checkpoints the WAL into the main file and
/// deletes the `-wal`/`-shm` sidecars. Verified locally — a read-write open of
/// an idle WAL database left only `c.db` behind, while the read-only open left
/// `a.db`, `a.db-wal` and `a.db-shm` untouched. A nightly checkpoint of a live
/// 385 MB database, forced out from under the agent that owns it, is exactly the
/// kind of write this check must never perform.
///
/// # WAL
///
/// Verified against a database in WAL mode with `wal_autocheckpoint=0`, the
/// writer connection still open and 100,000 rows living entirely in a 4 MB
/// uncheckpointed `-wal` (the main file was still 4096 bytes): the read-only
/// connection read *through* the WAL — `count(*)` returned 100000 and
/// `quick_check` returned `ok` — and all three file sizes were unchanged
/// afterwards. Hammering the same database from a writer thread during the check
/// showed no blocked commits and a worst-case commit latency of 6.6 ms, because
/// WAL readers and the single writer do not exclude each other.
///
/// If the sidecars are absent the read-only open *creates* them (a zero-length
/// `-wal` and a `-shm`) and leaves them behind, because a WAL reader must
/// register a read-mark in the wal-index. That is what any reader does, it does
/// not touch the database file, and the next writer reuses them. The one case it
/// bites is a database whose directory is not writable: SQLite then cannot create
/// the `-shm` and fails with SQLITE_READONLY (8), which is reported as
/// [`DbCheckOutcome::Unchecked`], never as corruption.
///
/// `immutable=1` would sidestep the sidecars entirely and is deliberately *not*
/// used: it disables locking and would read a torn snapshot of a live database,
/// manufacturing false corruption reports.
///
/// # Threading
///
/// The traversal runs in a separate OS process, so no tokio worker thread is
/// blocked by it — only this future is suspended awaiting the child.
pub async fn quick_check(program: &str, db: &Path) -> DbCheckOutcome {
    let mut cmd = Command::new(program);
    cmd.arg("-readonly")
        .arg(db)
        .arg("PRAGMA quick_check;")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return DbCheckOutcome::Unchecked(format!(
                "{program} not found on PATH — install sqlite3 to enable integrity checks"
            ));
        }
        Err(e) => return DbCheckOutcome::Unchecked(format!("could not run {program}: {e}")),
    };

    // On timeout the future is dropped, dropping the Child; `kill_on_drop` then
    // kills the traversal rather than leaving it running against a live file.
    let output = match tokio::time::timeout(CHECK_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return DbCheckOutcome::Unchecked(format!("{program} failed: {e}")),
        Err(_) => {
            return DbCheckOutcome::Unchecked(format!(
                "quick_check timed out after {}s",
                CHECK_TIMEOUT.as_secs()
            ));
        }
    };

    let report = String::from_utf8_lossy(&output.stdout);
    let report = report.trim();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();

    if output.status.success() && report == "ok" {
        return DbCheckOutcome::Ok;
    }

    // quick_check printed findings. It exits non-zero (SQLITE_CORRUPT) while
    // still writing a full report to stdout, so the report is checked first.
    if !report.is_empty() && report != "ok" {
        let detail: Vec<&str> = report.lines().take(MAX_DETAIL_LINES).collect();
        let elided = report.lines().count().saturating_sub(detail.len());
        let mut msg = detail.join("; ");
        if elided > 0 {
            msg.push_str(&format!("; (+{elided} more)"));
        }
        return DbCheckOutcome::Corrupt(msg);
    }

    // No report at all: SQLite refused the file before it could traverse it.
    // Damage to page 1 shows up here as SQLITE_NOTADB rather than as findings.
    if matches!(output.status.code(), Some(SQLITE_CORRUPT | SQLITE_NOTADB)) {
        return DbCheckOutcome::Corrupt(if stderr.is_empty() {
            format!(
                "sqlite3 rejected the file (exit {:?})",
                output.status.code()
            )
        } else {
            stderr.to_string()
        });
    }

    DbCheckOutcome::Unchecked(if stderr.is_empty() {
        format!("{program} exited with {}", output.status)
    } else {
        stderr.to_string()
    })
}

/// Outcome of one sweep over an agent's data directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepResult {
    pub checked: usize,
    pub corrupt: usize,
    pub unchecked: usize,
}

/// Check every database under `root`, logging each result.
///
/// Findings go to `tracing` **and** to the agent log buffer under the same
/// `[agent:<name>]` prefix the agent's own stderr uses, so a corrupt database
/// surfaces wherever a crash surfaces rather than dying inside a background
/// task. Nothing here stops, restarts or otherwise touches the agent: a corrupt
/// database is the worst possible moment to bounce a process, because the first
/// thing a restart does is reopen and write to the damaged file.
pub async fn sweep(log_buffer: &LogBuffer, agent: &str, root: &Path, program: &str) -> SweepResult {
    let databases = discover_databases(root);
    if databases.is_empty() {
        tracing::debug!(agent, root = %root.display(), "integrity check: no databases found");
        return SweepResult::default();
    }

    let prefix = format!("[agent:{agent}]");
    let mut result = SweepResult::default();

    for db in &databases {
        result.checked += 1;
        match quick_check(program, db).await {
            DbCheckOutcome::Ok => {
                tracing::debug!(agent, db = %db.display(), "quick_check ok");
            }
            DbCheckOutcome::Corrupt(detail) => {
                result.corrupt += 1;
                tracing::error!(
                    agent,
                    db = %db.display(),
                    detail = %detail,
                    "SQLITE CORRUPTION DETECTED — quick_check failed. The agent has NOT been \
                     stopped; restarting it would write to the damaged file. Take a copy of the \
                     database before doing anything else."
                );
                log_buffer.push(format!(
                    "{prefix} SQLITE CORRUPTION DETECTED in {} — quick_check: {detail}",
                    db.display()
                ));
            }
            DbCheckOutcome::Unchecked(reason) => {
                result.unchecked += 1;
                tracing::warn!(
                    agent,
                    db = %db.display(),
                    reason = %reason,
                    "integrity check could not run — this is not evidence the database is healthy"
                );
                log_buffer.push(format!(
                    "{prefix} integrity check skipped for {} — {reason} (not a clean bill of \
                     health)",
                    db.display()
                ));
            }
        }
    }

    // Logged even when everything passes: a check that silently stopped running
    // reproduces the exact failure mode this ticket exists to prevent.
    tracing::info!(
        agent,
        root = %root.display(),
        checked = result.checked,
        corrupt = result.corrupt,
        unchecked = result.unchecked,
        "sqlite integrity sweep complete"
    );

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn have_sqlite3() -> bool {
        std::process::Command::new(SQLITE3)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// Build a real SQLite database at `path`, in WAL mode, big enough to span
    /// many pages. Never touches anything outside the caller's temp directory.
    fn make_db(path: &Path, rows: usize) {
        let sql = format!(
            "PRAGMA journal_mode=WAL; \
             CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT); \
             INSERT INTO t(v) SELECT hex(randomblob(96)) FROM generate_series(1,{rows}); \
             CREATE INDEX idx_v ON t(v); \
             PRAGMA wal_checkpoint(TRUNCATE);"
        );
        let out = std::process::Command::new(SQLITE3)
            .arg(path)
            .arg(sql)
            .output()
            .expect("sqlite3 should run");
        assert!(
            out.status.success(),
            "fixture creation failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Copy `src` to `dst` and scribble over interior pages of the copy.
    ///
    /// Page 1 is left intact on purpose, so the copy still opens and still
    /// answers schema queries — that is the failure mode being reproduced.
    fn corrupt_copy(src: &Path, dst: &Path) {
        let mut bytes = std::fs::read(src).expect("read fixture");
        let page = 4096usize;
        assert!(
            bytes.len() > page * 80,
            "fixture too small to corrupt interior pages: {} bytes",
            bytes.len()
        );
        for pno in 20..70 {
            let off = pno * page;
            if off + page <= bytes.len() {
                bytes[off..off + page].fill(0xA5);
            }
        }
        std::fs::write(dst, &bytes).expect("write corrupt copy");
    }

    fn sqlite_query(db: &Path, sql: &str) -> (bool, String) {
        let out = std::process::Command::new(SQLITE3)
            .arg("-readonly")
            .arg(db)
            .arg(sql)
            .output()
            .expect("sqlite3 should run");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )
    }

    #[tokio::test]
    async fn test_quick_check_ok_on_healthy_wal_database() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        make_db(&db, 3000);

        assert_eq!(quick_check(SQLITE3, &db).await, DbCheckOutcome::Ok);
    }

    /// The ticket's verification step: corrupt a *copy* and confirm the check
    /// reports it — and confirm that an ordinary query on that same copy still
    /// succeeds, which is why an ordinary query can never stand in for this.
    #[tokio::test]
    async fn test_quick_check_detects_corruption_in_a_copy() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("state.db");
        let bad = dir.path().join("state-copy.db");
        make_db(&good, 3000);
        corrupt_copy(&good, &bad);

        assert_eq!(quick_check(SQLITE3, &good).await, DbCheckOutcome::Ok);

        let outcome = quick_check(SQLITE3, &bad).await;
        let DbCheckOutcome::Corrupt(detail) = &outcome else {
            panic!("expected Corrupt on the damaged copy, got {outcome:?}");
        };
        assert!(!detail.is_empty(), "corruption report should carry detail");

        // The trap, asserted rather than just commented: the damaged file still
        // opens and still answers a query with a perfectly plausible number.
        // Whether any *given* query survives is down to which pages the planner
        // visits, so no query result — high or low, count or max — is evidence
        // of integrity. Only the quick_check above is.
        let (ok, out) = sqlite_query(&bad, "SELECT count(*) FROM sqlite_master;");
        assert!(ok, "corrupt copy should still open and answer");
        assert!(
            out.parse::<i64>().is_ok_and(|n| n > 0),
            "expected a plausible row count from the corrupt file, got {out:?}"
        );
    }

    #[tokio::test]
    async fn test_missing_sqlite3_degrades_to_unchecked() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        std::fs::write(&db, b"").unwrap();

        let outcome = quick_check("rookery-no-such-sqlite3-binary", &db).await;
        match outcome {
            DbCheckOutcome::Unchecked(reason) => assert!(
                reason.contains("not found"),
                "expected a not-found reason, got {reason:?}"
            ),
            other => panic!("a missing checker must degrade, not report health: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_unreadable_file_is_unchecked_not_corrupt() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.db");
        // sqlite3 would happily create this if opened read-write; -readonly must not.
        match quick_check(SQLITE3, &missing).await {
            DbCheckOutcome::Unchecked(_) => {}
            other => panic!("a missing file is not a corruption finding: {other:?}"),
        }
        assert!(!missing.exists(), "the check must not create databases");
    }

    #[test]
    fn test_discover_finds_root_and_one_level_down_only() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        std::fs::write(root.join("state.db"), b"").unwrap();
        std::fs::write(root.join("memory_store.db"), b"").unwrap();
        // Sidecars must not be reported as separate databases.
        std::fs::write(root.join("state.db-wal"), b"").unwrap();
        std::fs::write(root.join("state.db-shm"), b"").unwrap();
        std::fs::write(root.join("config.yaml"), b"").unwrap();

        std::fs::create_dir_all(root.join("cron")).unwrap();
        std::fs::write(root.join("cron/executions.db"), b"").unwrap();

        // Two levels down: a backup snapshot tree, deliberately not walked.
        std::fs::create_dir_all(root.join("state-snapshots/20260719")).unwrap();
        std::fs::write(root.join("state-snapshots/20260719/state.db"), b"").unwrap();

        let found = discover_databases(root);
        let names: Vec<String> = found
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            names,
            vec!["cron/executions.db", "memory_store.db", "state.db"]
        );
    }

    #[test]
    fn test_discover_missing_directory_is_empty_not_a_panic() {
        assert!(discover_databases(Path::new("/rookery/nope/never")).is_empty());
    }

    #[tokio::test]
    async fn test_sweep_reports_corruption_to_the_log_buffer() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("memory_store.db");
        make_db(&good, 3000);

        let staging = tempfile::tempdir().unwrap();
        let template = staging.path().join("template.db");
        make_db(&template, 3000);
        corrupt_copy(&template, &dir.path().join("state.db"));

        let buffer = Arc::new(LogBuffer::new(64));
        let result = sweep(&buffer, "hermes", dir.path(), SQLITE3).await;

        assert_eq!(result.checked, 2);
        assert_eq!(result.corrupt, 1);
        assert_eq!(result.unchecked, 0);

        let lines = buffer.last_n(64);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("[agent:hermes]") && l.contains("SQLITE CORRUPTION")),
            "corruption must reach the agent log buffer, got {lines:?}"
        );
    }

    #[tokio::test]
    async fn test_sweep_on_healthy_directory_is_quiet() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        make_db(&dir.path().join("state.db"), 1000);

        let buffer = Arc::new(LogBuffer::new(64));
        let result = sweep(&buffer, "hermes", dir.path(), SQLITE3).await;

        assert_eq!(
            result,
            SweepResult {
                checked: 1,
                corrupt: 0,
                unchecked: 0
            }
        );
        assert!(buffer.last_n(64).is_empty(), "healthy sweep must not spam");
    }
}
