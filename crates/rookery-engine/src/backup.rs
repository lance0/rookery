//! Pre-change backups of a managed agent's SQLite databases.
//!
//! Rookery takes an agent down before an update and before a profile swap that
//! bounces it. Both of those are about to mutate state the agent owns —
//! `hermes update` applies config migrations in place — so this module takes a
//! copy first and keeps a few generations of them.
//!
//! It is the recovery half of [`crate::integrity`]: that module detects damage,
//! this one makes sure there is something to restore when it does. On
//! 2026-08-15 there wasn't, and 153 messages were hand-salvaged out of a corrupt
//! 385 MB file instead of restored from a copy.
//!
//! # Why not `cp`
//!
//! `cp` of a live database reads pages while the writer is mutating them and can
//! capture a torn page — producing exactly the file this module exists to avoid
//! creating. `VACUUM INTO` runs inside a read transaction, so the copy is a
//! consistent snapshot, and it is a single SQL statement rather than a CLI
//! dot-command.
//!
//! # WAL safety, verified on this box (sqlite3 3.45.1)
//!
//! The source is opened `-readonly`, and that is load-bearing rather than
//! hygiene. Measured against a database whose writer had been SIGKILLed, leaving
//! 2,867,552 bytes of uncheckpointed `-wal` and a 4,096-byte main file:
//!
//! - `sqlite3 db "VACUUM INTO ..."` (read-write) **deleted** `db-wal` and
//!   `db-shm` — it checkpointed the WAL on close, writing to a database rookery
//!   has no business writing to and which may be the damaged one.
//! - `sqlite3 -readonly db "VACUUM INTO ..."` left `db`, `db-wal`
//!   (2,867,552 bytes, unchanged) and `db-shm` untouched.
//!
//! Both copies contained all 20,000 rows even though every one of them lived
//! only in the WAL, confirming the reader reads *through* the WAL. The same held
//! with a writer connection still open and `wal_autocheckpoint=0`.
//!
//! `.backup` was measured as equivalent on both counts; `VACUUM INTO` is
//! preferred only because it cannot restart mid-copy when the source is busy.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::integrity::discover_databases;
use crate::logs::LogBuffer;

/// Subdirectory of the agent's data directory that holds backup generations.
pub const BACKUP_DIR: &str = "db-backups";

/// Appended to every copied file.
///
/// This is not cosmetic. [`discover_databases`] scans the data directory root
/// *and its immediate subdirectories* for `*.db`, so a backup written to
/// `<root>/db-backups/state.db` would be picked up by the LAN-1070 nightly
/// integrity sweep, multiplying its runtime by the retention count forever.
/// Backups are therefore protected twice over: they live one level deeper than
/// the sweep walks (inside a per-generation directory), *and* they do not end in
/// `.db`. Either alone would do; both means neither a deeper walk nor a change
/// to the extension filter can silently rope them in.
pub const BACKUP_SUFFIX: &str = ".bak";

/// How many generations to keep.
///
/// Worst case on disk is `KEEP_GENERATIONS x (total size of the agent's live
/// databases)` — for Hermes' ~400 MB that is ~1.2 GB. Three is enough to cover
/// "the update before last was already broken" without turning `~/.hermes` into
/// the disk-full error this box's 2026-08-15 incident first presented as.
const KEEP_GENERATIONS: usize = 3;

/// Per-file ceiling. `VACUUM INTO` of the real 385 MB `state.db` is seconds; this
/// only stops a pathological file from pinning a stop/update forever.
const BACKUP_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackupResult {
    pub copied: usize,
    pub failed: usize,
    /// Generations deleted by retention.
    pub pruned: usize,
}

/// Copy one database to `dest` with `VACUUM INTO`.
///
/// The source is opened read-only (see the module docs — a read-write open
/// checkpoints and deletes the WAL sidecars). `dest` must not already exist;
/// `VACUUM INTO` refuses to overwrite, which is the behaviour we want from a
/// backup.
///
/// Runs in a subprocess, so no tokio worker thread is blocked by the copy.
pub async fn backup_one(program: &str, db: &Path, dest: &Path) -> Result<(), String> {
    // The destination is a SQL string literal, so a single quote in the path has
    // to be doubled. Nothing in ~/.hermes has one; being wrong here would create
    // a file somewhere unexpected, so it is escaped anyway.
    let sql = format!(
        "VACUUM INTO '{}';",
        dest.display().to_string().replace('\'', "''")
    );

    let mut cmd = Command::new(program);
    cmd.arg("-readonly")
        .arg(db)
        .arg(sql)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{program} not found on PATH — install sqlite3 to enable database backups"
            ));
        }
        Err(e) => return Err(format!("could not run {program}: {e}")),
    };

    // On timeout the future is dropped, dropping the Child; `kill_on_drop` kills
    // the copy rather than leaving it running against a live file.
    let output = match tokio::time::timeout(BACKUP_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("{program} failed: {e}")),
        Err(_) => {
            return Err(format!(
                "backup timed out after {}s",
                BACKUP_TIMEOUT.as_secs()
            ));
        }
    };

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Err(if stderr.is_empty() {
        format!("{program} exited with {}", output.status)
    } else {
        stderr.to_string()
    })
}

/// Name of the generation directory to write now.
///
/// Millisecond precision so two stops in the same second do not collide, and a
/// fixed-width UTC layout so lexicographic order is chronological order — which
/// is the whole of [`prune`]'s sorting logic.
fn generation_name() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string()
}

/// Flatten a database path into one filename inside the generation directory.
///
/// `discover_databases` returns paths one level down as well as in the root, so
/// `cron/executions.db` and a root `executions.db` would otherwise write to the
/// same name and the second would fail as "output file already exists".
fn backup_file_name(root: &Path, db: &Path) -> String {
    let relative = db.strip_prefix(root).unwrap_or(db);
    format!(
        "{}{BACKUP_SUFFIX}",
        relative.to_string_lossy().replace(['/', '\\'], "_")
    )
}

/// Delete all but the newest `keep` generation directories under `dir`.
///
/// ponytail: plain blocking unlinks, not `spawn_blocking`. Removing three
/// directories of a handful of files each is a few dozen `unlink()` calls; the
/// expensive part of a backup is the `VACUUM INTO`, and that is a subprocess.
fn prune(dir: &Path, keep: usize) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut generations: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    generations.sort();

    let mut pruned = 0;
    let excess = generations.len().saturating_sub(keep);
    for old in generations.into_iter().take(excess) {
        match std::fs::remove_dir_all(&old) {
            Ok(()) => pruned += 1,
            Err(e) => {
                tracing::warn!(path = %old.display(), error = %e, "could not prune old db backup")
            }
        }
    }
    pruned
}

/// Back up every database under `root` into a fresh generation directory, then
/// prune to [`KEEP_GENERATIONS`].
///
/// Never returns an error and never panics: a backup failure is logged loudly to
/// `tracing` and to the agent log buffer, and the caller carries on. See
/// [`crate::agent::AgentManager::stop`] for why this fails open.
pub async fn run(
    log_buffer: &LogBuffer,
    agent: &str,
    root: &Path,
    program: &str,
    reason: &str,
) -> BackupResult {
    let databases = discover_databases(root);
    if databases.is_empty() {
        tracing::debug!(agent, root = %root.display(), "db backup: no databases found");
        return BackupResult::default();
    }

    let prefix = format!("[agent:{agent}]");
    let backups = root.join(BACKUP_DIR);
    let generation = backups.join(generation_name());

    if let Err(e) = std::fs::create_dir_all(&generation) {
        tracing::error!(
            agent,
            path = %generation.display(),
            error = %e,
            "COULD NOT CREATE DATABASE BACKUP DIRECTORY — proceeding without a pre-change copy"
        );
        log_buffer.push(format!(
            "{prefix} db backup FAILED before {reason}: cannot create {} — {e}",
            generation.display()
        ));
        return BackupResult {
            failed: databases.len(),
            ..Default::default()
        };
    }

    let mut result = BackupResult::default();
    for db in &databases {
        let dest = generation.join(backup_file_name(root, db));
        match backup_one(program, db, &dest).await {
            Ok(()) => result.copied += 1,
            Err(e) => {
                result.failed += 1;
                tracing::error!(
                    agent,
                    db = %db.display(),
                    error = %e,
                    "DATABASE BACKUP FAILED — the change is proceeding without a copy of this file"
                );
                log_buffer.push(format!(
                    "{prefix} db backup FAILED for {} before {reason} — {e}",
                    db.display()
                ));
            }
        }
    }

    result.pruned = prune(&backups, KEEP_GENERATIONS);

    tracing::info!(
        agent,
        reason,
        generation = %generation.display(),
        copied = result.copied,
        failed = result.failed,
        pruned = result.pruned,
        "sqlite pre-change backup complete"
    );

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrity::SQLITE3;
    use std::sync::Arc;

    fn have_sqlite3() -> bool {
        std::process::Command::new(SQLITE3)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// A real WAL database. Never touches anything outside the caller's tempdir.
    fn make_db(path: &Path, rows: usize) {
        let sql = format!(
            "PRAGMA journal_mode=WAL; \
             CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT); \
             INSERT INTO t(v) SELECT hex(randomblob(64)) FROM generate_series(1,{rows});"
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

    fn row_count(db: &Path) -> i64 {
        let out = std::process::Command::new(SQLITE3)
            .arg("-readonly")
            .arg(db)
            .arg("SELECT count(*) FROM t;")
            .output()
            .expect("sqlite3 should run");
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    #[tokio::test]
    async fn test_backup_one_produces_a_readable_copy() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        make_db(&db, 2000);
        let dest = dir.path().join("state.db.bak");

        backup_one(SQLITE3, &db, &dest).await.expect("backup");
        assert_eq!(row_count(&dest), 2000);
    }

    /// The constraint the whole module hangs off: the copy must not checkpoint
    /// the source's WAL away. A read-write open does exactly that, so this fails
    /// if the `-readonly` flag is ever dropped from `backup_one`.
    ///
    /// The fixture is a database whose writer was killed with an uncheckpointed
    /// WAL — the main file holds no rows at all, so the assertions also prove the
    /// backup read *through* the WAL rather than just copying the main file.
    #[tokio::test]
    async fn test_backup_does_not_checkpoint_or_delete_the_wal_sidecars() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let wal = dir.path().join("state.db-wal");

        // `wal_autocheckpoint=0` plus killing the writer leaves the rows in the
        // WAL. `.timeout` keeps the CLI alive on stdin so we can kill it before
        // it closes the connection (a clean close would checkpoint).
        let mut writer = std::process::Command::new(SQLITE3)
            .arg(&db)
            .arg(
                "PRAGMA journal_mode=WAL; \
                 PRAGMA wal_autocheckpoint=0; \
                 CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT); \
                 INSERT INTO t(v) SELECT hex(randomblob(64)) FROM generate_series(1,20000); \
                 SELECT 1; \
                 .timeout 60000",
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sqlite3 should run");
        // Wait for the WAL to appear rather than sleeping a fixed amount.
        for _ in 0..200 {
            if wal.exists() && std::fs::metadata(&wal).is_ok_and(|m| m.len() > 100_000) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = writer.kill();
        let _ = writer.wait();

        let wal_before = std::fs::metadata(&wal).expect("uncheckpointed wal").len();
        let main_before = std::fs::metadata(&db).unwrap().len();
        assert!(wal_before > 100_000, "fixture should have a fat wal");

        let dest = dir.path().join("copy.db.bak");
        backup_one(SQLITE3, &db, &dest).await.expect("backup");

        assert!(wal.exists(), "backup must not delete the -wal sidecar");
        assert_eq!(
            std::fs::metadata(&wal).unwrap().len(),
            wal_before,
            "backup must not checkpoint the source's WAL"
        );
        assert_eq!(
            std::fs::metadata(&db).unwrap().len(),
            main_before,
            "backup must not write to the source database"
        );
        assert_eq!(
            row_count(&dest),
            20000,
            "backup must read through the WAL, not just copy the main file"
        );
    }

    #[tokio::test]
    async fn test_backup_refuses_to_overwrite_and_reports_it() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        make_db(&db, 100);
        let dest = dir.path().join("state.db.bak");

        backup_one(SQLITE3, &db, &dest).await.expect("first");
        let err = backup_one(SQLITE3, &db, &dest)
            .await
            .expect_err("a backup must never clobber an existing one");
        assert!(!err.is_empty());
    }

    #[tokio::test]
    async fn test_missing_sqlite3_degrades_to_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        std::fs::write(&db, b"").unwrap();
        let err = backup_one("rookery-no-such-sqlite3-binary", &db, &dir.path().join("x"))
            .await
            .expect_err("a missing binary is a failure, not a silent success");
        assert!(err.contains("not found"), "got {err:?}");
    }

    /// The trap called out in the ticket: backups must be invisible to the
    /// LAN-1070 nightly integrity sweep, which scans the root and every
    /// immediate subdirectory for `*.db`.
    #[tokio::test]
    async fn test_backups_are_not_picked_up_by_the_integrity_sweep() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_db(&root.join("state.db"), 500);
        std::fs::create_dir_all(root.join("cron")).unwrap();
        make_db(&root.join("cron/executions.db"), 100);

        let before = discover_databases(root);
        assert_eq!(before.len(), 2);

        let buffer = Arc::new(LogBuffer::new(64));
        let result = run(&buffer, "hermes", root, SQLITE3, "test").await;
        assert_eq!(result.copied, 2);
        assert_eq!(result.failed, 0);

        assert_eq!(
            discover_databases(root),
            before,
            "the integrity sweep must not start scanning our backups"
        );

        // The two protections asserted separately, so neither can quietly rot:
        // depth (the sweep walks root + one level; backups are two levels down)
        // and extension (nothing we write ends in `.db`). This second assertion
        // is what fails if BACKUP_SUFFIX is ever dropped.
        assert!(
            discover_databases(&root.join(BACKUP_DIR)).is_empty(),
            "even a sweep rooted at the backup directory must find no *.db"
        );
    }

    /// Same-named databases at different depths must not collide.
    #[tokio::test]
    async fn test_nested_database_with_a_duplicate_name_does_not_collide() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_db(&root.join("state.db"), 300);
        std::fs::create_dir_all(root.join("cron")).unwrap();
        make_db(&root.join("cron/state.db"), 700);

        let buffer = Arc::new(LogBuffer::new(64));
        let result = run(&buffer, "hermes", root, SQLITE3, "test").await;
        assert_eq!(result.copied, 2, "both databases must be copied");
        assert_eq!(result.failed, 0);

        let generation = std::fs::read_dir(root.join(BACKUP_DIR))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut names: Vec<String> = std::fs::read_dir(&generation)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["cron_state.db.bak", "state.db.bak"]);
        assert_eq!(row_count(&generation.join("state.db.bak")), 300);
        assert_eq!(row_count(&generation.join("cron_state.db.bak")), 700);
    }

    /// Retention: without pruning this grows without bound, which is the failure
    /// mode the 2026-08-15 incident first presented as.
    #[tokio::test]
    async fn test_retention_keeps_only_the_newest_generations() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_db(&root.join("state.db"), 200);
        let buffer = Arc::new(LogBuffer::new(64));

        for _ in 0..KEEP_GENERATIONS + 2 {
            run(&buffer, "hermes", root, SQLITE3, "test").await;
            // generation_name() has millisecond resolution; make sure two runs
            // cannot land on the same name.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let generations = std::fs::read_dir(root.join(BACKUP_DIR)).unwrap().count();
        assert_eq!(generations, KEEP_GENERATIONS);
    }

    #[test]
    fn test_prune_is_chronological_and_a_noop_below_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in ["20260101T000000000Z", "20260819T120000000Z", "20260820T"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        assert_eq!(prune(root, 5), 0, "nothing to prune below the limit");

        assert_eq!(prune(root, 1), 2);
        let left: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["20260820T"], "the newest must survive");
    }

    #[tokio::test]
    async fn test_empty_directory_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let buffer = Arc::new(LogBuffer::new(64));
        let result = run(&buffer, "hermes", dir.path(), SQLITE3, "test").await;
        assert_eq!(result, BackupResult::default());
        assert!(
            !dir.path().join(BACKUP_DIR).exists(),
            "no databases means no backup directory"
        );
    }
}
