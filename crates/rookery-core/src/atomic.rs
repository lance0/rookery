//! Durable atomic file replacement.
//!
//! `write` + `rename` alone is atomic against a *process* crash — rename(2) either
//! happens or it doesn't, so a reader never sees a torn file. It is **not** durable
//! against power loss: the rename can reach disk before the temp file's contents do,
//! leaving a zero-length or truncated file where valid data used to be.
//!
//! That matters here because these files are what let the daemon re-adopt a running
//! inference server across a restart. An unreadable `state.json` makes the daemon
//! forget the server it was managing, and the orphan reaper then SIGKILLs a healthy
//! 30 GB process.
//!
//! Ordering below: write temp -> fsync temp -> rename -> fsync parent dir. The final
//! directory fsync is what makes the rename itself durable; without it the directory
//! entry can still be lost.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

/// Replace `path` with `contents`, durably.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Distinct suffix per path so concurrent writers to different files never collide.
    let tmp = path.with_extension("tmp");

    {
        let mut f = File::create(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?; // contents durable before anything points at them
    }

    fs::rename(&tmp, path)?;

    // Make the rename itself durable. Best-effort: some filesystems refuse to open a
    // directory for sync, and failing the whole write over that would be worse than
    // the residual risk.
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");

        write_atomic(&p, b"first").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "first");

        write_atomic(&p, b"second").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "second");
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a").join("b").join("s.json");

        write_atomic(&p, b"x").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "x");
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        write_atomic(&p, b"x").unwrap();

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file was not cleaned up");
    }
}
