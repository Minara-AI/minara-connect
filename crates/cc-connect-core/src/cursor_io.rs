//! Cursor I/O — per-(Room, Session) marker of last-seen Message id.
//!
//! See `PROTOCOL.md` §9 (Cursor format) and §7.3 step 8 (atomic advance with
//! flock-vs-rename race protocol).
//!
//! - Path: `~/.cc-connect/cursors/<topic_id_hex>/<session_id>.cursor`
//! - File mode `0600`, parent dir mode `0700`.
//! - Empty / missing file = no Messages yet seen.
//! - v0.1 wire form: a single line containing one ULID (no trailing whitespace).
//! - v0.2+ tolerated read form: JSON `{"v":1,"id":"..."}` (forward-compat).
//! - Advance: write to sibling `.cursor.tmp` → fsync → rename → fsync parent.

use crate::posix::{acquire_lock, release_lock, LockKind};
use anyhow::{anyhow, bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Maximum retries for the open-stat-vs-fstat rename race in `advance_cursor`.
const RACE_RETRY_LIMIT: usize = 5;

/// Read the cursor at `path`. Returns:
///   - `Ok(None)` if the file is missing or empty.
///   - `Ok(Some(ulid))` for a bare-ULID line (v0.1 form).
///   - `Ok(Some(ulid))` for a JSON object form (`{"v":1,"id":"…"}`, v0.2+ forward-compat).
///   - `Err(_)` for malformed JSON or unreadable file.
///
/// A trailing `\n` is tolerated. The returned ULID string is **not** Crockford-
/// normalised here (callers that need normalisation should run `message::normalize_ulid`).
pub fn read_cursor(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read cursor {}", path.display()))?;
    let trimmed = raw.trim_end_matches('\n').trim_end_matches('\r');
    if trimmed.is_empty() {
        return Ok(None);
    }

    if trimmed.starts_with('{') {
        // PROTOCOL.md §9 forward-compat: JSON `{ "v": 1, "id": "<ULID>" }`.
        #[derive(serde::Deserialize)]
        struct CursorJson {
            id: String,
        }
        let parsed: CursorJson = serde_json::from_str(trimmed)
            .map_err(|e| anyhow!("CURSOR_PARSE_ERROR: malformed JSON cursor: {e}"))?;
        return Ok(Some(parsed.id));
    }

    Ok(Some(trimmed.to_string()))
}

/// Atomically advance the cursor at `path` to `new_ulid`.
///
/// Implements the §7.3 step 8 protocol exactly:
///   1. Ensure parent directory exists with mode `0700`.
///   2. Open the cursor with `O_RDWR | O_CREAT`, mode `0600`.
///   3. Acquire `LOCK_EX`.
///   4. After the lock, compare the path's inode to the held fd's inode
///      (`stat` vs `fstat`). If they differ, drop the lock, close, re-open,
///      and retry — up to `RACE_RETRY_LIMIT` attempts.
///   5. Write `new_ulid` to a sibling `.cursor.tmp`, fsync the tmp file,
///      then `rename(2)` over the canonical path, then fsync the parent dir.
///   6. Release the lock and close.
pub fn advance_cursor(path: &Path, new_ulid: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }

    for attempt in 1..=RACE_RETRY_LIMIT {
        let file = open_cursor_rwc(path)?;
        acquire_lock(&file, LockKind::Exclusive)?;

        if path_matches_fd(path, &file)? {
            let result = write_atomically(path, new_ulid);
            // Lock released automatically when `file` drops, but be explicit
            // for clarity around the rename ordering.
            let _ = release_lock(&file);
            drop(file);
            return result;
        }

        // Race: another writer renamed `path` between our open and our lock
        // acquisition. Drop everything and retry.
        let _ = release_lock(&file);
        drop(file);
        if attempt == RACE_RETRY_LIMIT {
            bail!(
                "CURSOR_RACE_RETRY_EXHAUSTED: {} attempts and path inode still differs from held fd",
                RACE_RETRY_LIMIT
            );
        }
    }
    unreachable!("loop returns or bails before falling through");
}

fn open_cursor_rwc(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open cursor {}", path.display()))
}

/// Return whether the inode currently at `path` matches the inode the open
/// file descriptor `file` is referring to. The comparison uses both `dev`
/// and `ino` so we don't false-match across mounts.
fn path_matches_fd(path: &Path, file: &File) -> Result<bool> {
    let path_meta = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?;
    let fd_meta = file.metadata().context("fstat held cursor fd")?;
    Ok(path_meta.dev() == fd_meta.dev() && path_meta.ino() == fd_meta.ino())
}

fn write_atomically(path: &Path, new_ulid: &str) -> Result<()> {
    let (mut tmp_file, tmp_path) = create_unique_tmp(path)?;
    // PROTOCOL.md §9: bare ULID, no trailing whitespace.
    tmp_file
        .write_all(new_ulid.as_bytes())
        .with_context(|| format!("write tmp {}", tmp_path.display()))?;
    tmp_file.sync_all().context("fsync tmp")?;
    drop(tmp_file);

    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("rename {} → {}", tmp_path.display(), path.display()))?;

    // PROTOCOL.md §7.3 step 8: fsync the parent directory so the rename
    // survives a crash.
    if let Some(parent) = path.parent() {
        if let Ok(parent_dir) = File::open(parent) {
            let _ = parent_dir.sync_all();
        }
    }
    Ok(())
}

/// Create a uniquely-named tmp file in the same directory as `path`.
///
/// Concurrent advances need disjoint tmp files; otherwise one writer's
/// `rename(2)` can pull the path out from under another, leaving the
/// second to fail with `ENOENT`. We use `O_CREAT | O_EXCL` with a random
/// suffix and retry on the unlikely collision.
fn create_unique_tmp(path: &Path) -> Result<(File, PathBuf)> {
    const ATTEMPTS: usize = 10;
    for _ in 0..ATTEMPTS {
        let mut suffix_bytes = [0u8; 8];
        getrandom::getrandom(&mut suffix_bytes)
            .map_err(|e| anyhow!("OS random for tmp suffix: {e}"))?;
        let suffix: String = suffix_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let tmp_path = path.with_extension(format!("cursor.tmp.{suffix}"));

        match OpenOptions::new()
            .create_new(true) // O_CREAT | O_EXCL
            .write(true)
            .mode(0o600)
            .open(&tmp_path)
        {
            Ok(f) => return Ok((f, tmp_path)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(anyhow!("open tmp {}: {e}", tmp_path.display()));
            }
        }
    }
    bail!("CURSOR_TMP_COLLISION: {ATTEMPTS} random suffixes all collided (improbable, investigate)");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn missing_cursor_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.cursor");
        assert_eq!(read_cursor(&path).unwrap(), None);
    }

    #[test]
    fn empty_cursor_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.cursor");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(read_cursor(&path).unwrap(), None);
    }

    #[test]
    fn empty_cursor_with_only_newline_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("just_newline.cursor");
        std::fs::write(&path, b"\n").unwrap();
        assert_eq!(read_cursor(&path).unwrap(), None);
    }

    #[test]
    fn advance_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.cursor");
        let ulid = "01HZA8K9F0RS3JXG7QZ4N5VTBC";
        advance_cursor(&path, ulid).unwrap();
        assert_eq!(read_cursor(&path).unwrap(), Some(ulid.to_string()));
    }

    #[test]
    fn advance_overwrites_previous_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.cursor");
        let v1 = "01HZA000000000000000000001";
        let v2 = "01HZA000000000000000000002";
        advance_cursor(&path, v1).unwrap();
        advance_cursor(&path, v2).unwrap();
        assert_eq!(read_cursor(&path).unwrap(), Some(v2.to_string()));
    }

    #[test]
    fn cursor_file_has_no_trailing_whitespace_on_emit() {
        // PROTOCOL.md §9 writers MUST NOT emit trailing whitespace.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.cursor");
        let ulid = "01HZA8K9F0RS3JXG7QZ4N5VTBC";
        advance_cursor(&path, ulid).unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(raw, ulid.as_bytes(), "cursor MUST be the bare ULID, no whitespace");
    }

    #[test]
    fn cursor_file_has_mode_0600_after_advance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.cursor");
        advance_cursor(&path, "01HZA8K9F0RS3JXG7QZ4N5VTBC").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "PROTOCOL §9: cursor file mode 0600");
    }

    #[test]
    fn parent_directory_is_created_with_mode_0700() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("cursors").join("topic_aaaa");
        let path = parent.join("session_xxx.cursor");
        advance_cursor(&path, "01HZA8K9F0RS3JXG7QZ4N5VTBC").unwrap();
        let mode = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "PROTOCOL §9: cursor parent dir mode 0700");
    }

    /// PROTOCOL.md §11.7: bare-ULID and JSON forms must yield the same value.
    #[test]
    fn protocol_11_7_bare_ulid_form() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bare.cursor");
        std::fs::write(&path, b"01HZA8K9F0RS3JXG7QZ4N5VTBC").unwrap();
        assert_eq!(
            read_cursor(&path).unwrap(),
            Some("01HZA8K9F0RS3JXG7QZ4N5VTBC".to_string())
        );
    }

    #[test]
    fn protocol_11_7_bare_ulid_with_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bare_nl.cursor");
        std::fs::write(&path, b"01HZA8K9F0RS3JXG7QZ4N5VTBC\n").unwrap();
        assert_eq!(
            read_cursor(&path).unwrap(),
            Some("01HZA8K9F0RS3JXG7QZ4N5VTBC".to_string())
        );
    }

    #[test]
    fn protocol_11_7_json_form_forward_compat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("json.cursor");
        std::fs::write(
            &path,
            br#"{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC"}"#,
        )
        .unwrap();
        assert_eq!(
            read_cursor(&path).unwrap(),
            Some("01HZA8K9F0RS3JXG7QZ4N5VTBC".to_string())
        );
    }

    #[test]
    fn malformed_json_cursor_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.cursor");
        std::fs::write(&path, b"{not json").unwrap();
        let err = read_cursor(&path).err().expect("malformed JSON must error");
        assert!(
            err.to_string().contains("CURSOR_PARSE_ERROR"),
            "got: {err}"
        );
    }

    /// Concurrent advances MUST end with the file containing exactly one of
    /// the proposed values, and the file MUST be well-formed (no torn writes,
    /// no leftover .cursor.tmp).
    #[test]
    fn concurrent_advances_serialise() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("serialised.cursor"));

        const N: usize = 16;
        let mut handles = Vec::new();
        for i in 0..N {
            let path = Arc::clone(&path);
            handles.push(std::thread::spawn(move || {
                // Each thread proposes a deterministic ULID-shaped value.
                let ulid = format!("01HZA{:021}", i);
                advance_cursor(&path, &ulid).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // The final cursor must be one of the N proposed values.
        let final_value = read_cursor(&path).unwrap().expect("cursor present");
        assert!(
            (0..N).any(|i| format!("01HZA{:021}", i) == final_value),
            "final cursor {final_value} should be one of the proposals"
        );

        // No leftover `*.cursor.tmp.*` should remain after all advances complete.
        let parent = path.parent().expect("path has parent");
        let leftovers: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains(".cursor.tmp.")
            })
            .map(|e| e.path())
            .collect();
        assert!(
            leftovers.is_empty(),
            "leftover tmp files after concurrent advances: {leftovers:?} \
             — a write was not finalised by rename"
        );
    }
}
