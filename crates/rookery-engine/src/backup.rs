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
//!
//! ## A read-only open *can* recover a hot WAL, and says so when it cannot
//!
//! The worry that motivates [`verify`] below is that `-readonly` cannot rebuild
//! the `-shm` wal-index a crashed writer left stale or missing, and would then
//! quietly copy only the main file. Measured, it does not behave that way:
//!
//! - hot WAL, `-shm` intact — copy has all 20,000 rows.
//! - hot WAL, `-shm` **deleted** — the read-only open *recreates* it, recovers
//!   the WAL, and the copy has all 20,000 rows.
//! - hot WAL, `-shm` overwritten with 32 KB of random bytes — same, 20,000 rows.
//! - hot WAL, `-shm` deleted **and the directory made unwritable**, so the
//!   wal-index genuinely cannot be built — `Error: stepping, unable to open
//!   database file (14)`, exit 14, **and no destination file is produced**.
//!
//! That last case is the important one: SQLite cannot locate WAL frames without
//! a wal-index, so there is no mode in which it silently ignores a valid WAL and
//! returns main-file-only data. It fails with SQLITE_CANTOPEN instead. A silent
//! partial copy is therefore not reachable through the WAL path.
//!
//! # What *is* reachable: a partial file left behind by a failed copy
//!
//! `VACUUM INTO` creates its destination immediately and fills it as it goes, so
//! every way a copy can fail leaves a plausible-looking `.bak` on disk:
//!
//! - the copy is killed part-way (our timeout, or the box going down) —
//!   measured: a 512,000-byte truncated `.bak` **and** a `.bak-journal`.
//! - the source is corrupt — measured: exit 11, and a **0-byte** `.bak`.
//! - the destination filesystem fills up — SQLITE_FULL, same shape.
//!
//! A truncated backup that looks like a backup is the failure this module exists
//! to prevent, so [`backup_one`] discards its output on any failure and only
//! reports success once the copy has been read back and verified. Note that a
//! 0-byte file passes `PRAGMA quick_check` ("ok" — SQLite reads it as a valid
//! empty database), which is why the discard-on-error path is not redundant with
//! the verification: neither check subsumes the other.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::integrity::{DbCheckOutcome, discover_databases, quick_check};
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

/// Back up one database to `dest`, or leave nothing behind.
///
/// The source is opened read-only (see the module docs — a read-write open
/// checkpoints and deletes the WAL sidecars).
///
/// On success `dest` is a copy that has been read back and passed
/// `PRAGMA quick_check`. On failure `dest` does not exist: a `VACUUM INTO` that
/// dies part-way leaves a truncated file, and a truncated file that looks like a
/// backup is worse than an obvious absence of one.
pub async fn backup_one(program: &str, db: &Path, dest: &Path) -> Result<(), String> {
    // Establishing that we are the only writer of this path is what makes the
    // discards below safe: they can only ever delete a file this call created.
    // `VACUUM INTO` refuses to overwrite too, but by then its failure is
    // indistinguishable from one we caused, and cleaning up would delete a
    // perfectly good older backup.
    if dest.exists() {
        return Err(format!(
            "refusing to overwrite an existing backup at {}",
            dest.display()
        ));
    }

    if let Err(e) = copy_into(program, db, dest).await {
        discard(dest);
        return Err(e);
    }

    match verify(program, dest).await {
        Ok(()) => Ok(()),
        Err(e) => {
            discard(dest);
            Err(e)
        }
    }
}

/// Read the finished copy back and confirm it is a whole database.
///
/// Cheap insurance against a `VACUUM INTO` that exits 0 having written a file
/// SQLite cannot read — roughly a second per 200 MB, against a copy that is
/// itself seconds, on a path that only runs before an update or a swap.
///
/// `Unchecked` is treated as failure, not as a pass. It means the copy could not
/// be confirmed whole, and this module's entire premise is that an unverified
/// backup is not worth keeping. It cannot be the "sqlite3 is missing" case here,
/// because the copy that produced the file just ran the same binary.
async fn verify(program: &str, dest: &Path) -> Result<(), String> {
    match quick_check(program, dest).await {
        DbCheckOutcome::Ok => Ok(()),
        DbCheckOutcome::Corrupt(detail) => Err(format!("the copy did not verify: {detail}")),
        DbCheckOutcome::Unchecked(detail) => {
            Err(format!("the copy could not be verified: {detail}"))
        }
    }
}

/// Delete a backup that failed, and the sidecars an interrupted copy leaves.
///
/// A killed `VACUUM INTO` leaves both a truncated `dest` and a `dest-journal`.
/// The `-wal`/`-shm` names cost two array entries and mean this still cleans up
/// whole if the destination journal mode ever changes.
fn discard(dest: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut path = dest.as_os_str().to_owned();
        path.push(suffix);
        let path = PathBuf::from(path);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::error!(
                path = %path.display(),
                error = %e,
                "COULD NOT REMOVE A FAILED DATABASE BACKUP — a partial copy may remain on disk"
            ),
        }
    }
}

/// Run the `VACUUM INTO` itself, in a subprocess so no tokio worker thread is
/// blocked by the copy.
async fn copy_into(program: &str, db: &Path, dest: &Path) -> Result<(), String> {
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

    /// Leave `path` as a database whose writer died with everything still in an
    /// uncheckpointed WAL: `rows` committed rows, a fat `-wal`, and a main file
    /// that holds none of them.
    ///
    /// # Why the SQL goes in on stdin
    ///
    /// The writer has to be SIGKILLed *after* its INSERT commits and *before* it
    /// closes the connection, because a clean close checkpoints the WAL away.
    /// Both halves of that are ordering, not timing, and the fixture is built so
    /// that neither depends on how fast the machine is:
    ///
    /// - **Committed**: `SELECT 'ROOKERY-COMMITTED'` cannot run until the INSERT
    ///   ahead of it has finished, and the CLI flushes each statement's output,
    ///   so reading that line off stdout means the rows are durable in the WAL.
    /// - **Still alive**: with the statements fed on stdin and stdin left open,
    ///   the CLI blocks waiting for another statement. Passing them as an
    ///   argument instead makes it exit as soon as they are done, racing the kill.
    ///
    /// The previous version of this waited for the `-wal` to exceed 100 KB, which
    /// is a race: the committed WAL for 20,000 rows is 2,867,552 bytes, so that
    /// threshold trips about 3.5% of the way in. On a fast box the whole INSERT
    /// finishes inside one 25 ms poll interval and the fixture always wins; on a
    /// loaded CI runner a sample lands mid-INSERT, the kill discards an
    /// uncommitted transaction, and the backup is correctly empty — which read as
    /// "backup must read through the WAL" and turned CI red. Reproduced here by
    /// running the old fixture under CPU contention: 1 in 12 runs killed the
    /// writer at 1,945,600 bytes and recovered 0 rows.
    fn hot_wal_writer(path: &Path, rows: usize) {
        use std::io::{BufRead, BufReader, Write};

        let mut writer = std::process::Command::new(SQLITE3)
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("sqlite3 should run");

        let mut stdin = writer.stdin.take().expect("piped stdin");
        write!(
            stdin,
            "PRAGMA journal_mode=WAL;\n\
             PRAGMA wal_autocheckpoint=0;\n\
             CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);\n\
             INSERT INTO t(v) SELECT hex(randomblob(64)) FROM generate_series(1,{rows});\n\
             SELECT 'ROOKERY-COMMITTED';\n"
        )
        .expect("sqlite3 should accept statements");
        stdin.flush().expect("flush");
        // Deliberately NOT dropped: closing stdin would let the CLI exit cleanly
        // and checkpoint the WAL into the main file.

        let committed = BufReader::new(writer.stdout.take().expect("piped stdout"))
            .lines()
            .map_while(Result::ok)
            .any(|line| line.trim() == "ROOKERY-COMMITTED");

        let _ = writer.kill();
        let _ = writer.wait();
        assert!(
            committed,
            "fixture writer died before committing — the rest of this test would be meaningless"
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

        hot_wal_writer(&db, 20000);
        let wal_before = std::fs::metadata(&wal).expect("uncheckpointed wal").len();
        let main_before = std::fs::metadata(&db).unwrap().len();
        assert!(wal_before > 100_000, "fixture should have a fat wal");
        assert!(
            main_before <= 8192,
            "fixture is only meaningful if the main file holds no rows: {main_before} bytes"
        );

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
    async fn test_backup_refuses_to_overwrite_and_leaves_the_existing_one_intact() {
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
        // The refusal has to come *before* the copy runs, because every failure
        // path now cleans up after itself: reaching the cleanup with a file we
        // did not create would delete a good backup on a failed retry.
        assert_eq!(
            row_count(&dest),
            100,
            "the pre-existing backup must survive a refused overwrite"
        );
    }

    /// `VACUUM INTO` creates its destination up front and fills it as it goes, so
    /// every failure leaves something behind: measured, a corrupt source exits 11
    /// having created a 0-byte `.bak`, and a copy killed part-way leaves a
    /// truncated one plus a `.bak-journal`. A file that looks like a backup but
    /// is not one is the exact failure this module exists to prevent, so nothing
    /// may survive a failed copy.
    ///
    /// Truncating a valid database is the failure mode used here because it is
    /// deterministic on any sqlite3 build — the header describes more pages than
    /// the file holds, and `VACUUM INTO` reads every page.
    #[tokio::test]
    async fn test_a_failed_backup_leaves_no_partial_file_behind() {
        if !have_sqlite3() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        make_db(&db, 20000);
        let full = std::fs::metadata(&db).unwrap().len();
        assert!(full > 1_000_000, "fixture should be several hundred pages");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&db)
            .unwrap()
            .set_len(262_144)
            .unwrap();

        let dest = dir.path().join("state.db.bak");
        let err = backup_one(SQLITE3, &db, &dest)
            .await
            .expect_err("a truncated source cannot be copied whole");
        assert!(!err.is_empty());
        assert!(
            !dest.exists(),
            "a partial backup must never survive a failed copy (error was: {err})"
        );
    }

    /// `discard` has to remove the journal an interrupted `VACUUM INTO` leaves
    /// next to its output, and nothing else. It runs on failure paths where a
    /// wrong deletion would destroy a good backup, so the blast radius is
    /// pinned here rather than left to inspection.
    #[test]
    fn test_discard_removes_the_copy_and_its_sidecars_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("state.db.bak");
        std::fs::write(&dest, b"truncated copy").unwrap();
        std::fs::write(dir.path().join("state.db.bak-journal"), b"hot journal").unwrap();
        let neighbour = dir.path().join("cron_state.db.bak");
        std::fs::write(&neighbour, b"a different database's backup").unwrap();
        let prefixed = dir.path().join("state.db.bak.keep");
        std::fs::write(&prefixed, b"not a sidecar").unwrap();

        discard(&dest);

        assert!(!dest.exists(), "the failed copy must go");
        assert!(
            !dir.path().join("state.db.bak-journal").exists(),
            "the journal an interrupted copy leaves must go with it"
        );
        assert!(neighbour.exists(), "other backups must be untouched");
        assert!(
            prefixed.exists(),
            "only the exact sidecar suffixes, not everything sharing the prefix"
        );

        // Idempotent: `run` keeps going after a failure, and a second discard of
        // an already-clean path must not log or panic.
        discard(&dest);
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
