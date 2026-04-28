//! Append-only JSONL chat log per (Room, machine).
//!
//! See `PROTOCOL.md` §5 (Chat log) and §6.1 (atomic-append discipline).
//!
//! Each log file is `~/.cc-connect/rooms/<topic_id_hex>/log.jsonl`. One
//! Message per line. Writers serialise with an `fcntl(F_OFD_SETLK)`
//! exclusive lock; readers take a shared lock.

use crate::message::Message;
use crate::posix::{acquire_lock, release_lock, LockKind};
use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Open the log file at `path`, creating the file (mode `0600`) and any
/// missing parent directories (mode `0700`) if needed.
pub fn open_or_create_log(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
            // PROTOCOL.md §5: parent dir mode 0700.
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open log file {}", path.display()))?;
    Ok(file)
}

/// Append a single Message to the log atomically.
///
/// The Message is canonical-JSON encoded (PROTOCOL.md §4) and terminated
/// by a single `\n`. The full payload (JSON + newline) is written in **one**
/// `write(2)` syscall to a file opened with `O_APPEND`; for any payload that
/// fits in a single kernel write — which all messages do, since canonical
/// JSON is bounded by `BODY_MAX_BYTES + ~150 envelope bytes ≪ 16 KiB` —
/// the write is atomic relative to other O_APPEND writers regardless of
/// lock semantics. We additionally hold an exclusive fcntl lock across the
/// write+fsync window for cross-process visibility (single-machine readers
/// and writers serialise against each other).
pub fn append(file: &mut File, msg: &Message) -> Result<()> {
    let mut payload = msg.to_canonical_json()?;

    // Sanity: canonical JSON cannot contain a raw newline (PROTOCOL §5
    // requires "exactly one `\n` byte" per line, and that byte is the
    // terminator we add). serde_json escapes 0x0A as `\n`, so this branch
    // is a defensive invariant check.
    if payload.contains(&b'\n') {
        bail!("APPEND_INVARIANT: canonical JSON unexpectedly contains a raw newline byte");
    }
    payload.push(b'\n');

    acquire_lock(file, LockKind::Exclusive)?;

    let result = (|| -> Result<()> {
        // Single-syscall write for atomicity under O_APPEND.
        file.write_all(&payload)?;
        file.sync_all()?;
        Ok(())
    })();

    let _ = release_lock(file);
    result
}

/// Read all Messages from the log whose `id > cursor`, or every Message
/// if `cursor` is `None`. Skips and warns on malformed lines (PROTOCOL §5
/// "corruption tolerance").
///
/// Acquires a shared fcntl OFD lock for the duration of the scan; concurrent
/// readers are permitted, concurrent writers block until we release.
pub fn read_since(file: &mut File, cursor: Option<&str>) -> Result<Vec<Message>> {
    acquire_lock(file, LockKind::Shared)?;

    let result = (|| -> Result<Vec<Message>> {
        file.seek(SeekFrom::Start(0))?;
        let reader = BufReader::new(&*file);
        let mut out = Vec::new();
        for (lineno, line_result) in reader.lines().enumerate() {
            let line = match line_result {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("log_io: line {} read error: {e}", lineno + 1);
                    continue;
                }
            };
            if line.is_empty() {
                continue;
            }
            let msg = match Message::from_wire_bytes(line.as_bytes()) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("log_io: line {} parse error: {e}", lineno + 1);
                    continue;
                }
            };
            if let Some(c) = cursor {
                // PROTOCOL §5 + §9: cursor is exclusive; only ids strictly > cursor pass.
                if msg.id.as_str() <= c {
                    continue;
                }
            }
            out.push(msg);
        }
        Ok(out)
    })();

    let _ = release_lock(file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::Arc;

    fn make_msg(id: &str, body: &str) -> Message {
        Message::new(
            id,
            "hnvcppgow2sc2yvdvdicu3ynonsteflxdxrehjr2ybekdc2z3iuq".to_string(),
            1714000000000,
            body.to_string(),
        )
        .expect("valid msg")
    }

    #[test]
    fn create_log_sets_modes() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("rooms").join("aaaa").join("log.jsonl");
        let _f = open_or_create_log(&log_path).unwrap();

        let file_mode = std::fs::metadata(&log_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "PROTOCOL §5: log file mode 0600");

        let parent_mode = std::fs::metadata(log_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700, "PROTOCOL §5: parent dir mode 0700");
    }

    #[test]
    fn append_then_read_returns_message() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("log.jsonl");
        let mut f = open_or_create_log(&log_path).unwrap();

        let msg = make_msg("01HZA8K9F0RS3JXG7QZ4N5VTBC", "hello");
        append(&mut f, &msg).unwrap();

        let read = read_since(&mut f, None).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0], msg);
    }

    #[test]
    fn append_writes_line_with_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("log.jsonl");
        let mut f = open_or_create_log(&log_path).unwrap();
        append(&mut f, &make_msg("01HZA8K9F0RS3JXG7QZ4N5VTBC", "x")).unwrap();
        let raw = std::fs::read(&log_path).unwrap();
        assert!(
            raw.ends_with(b"\n"),
            "PROTOCOL §5: each line MUST end with `\\n`"
        );
        assert_eq!(raw.iter().filter(|&&b| b == b'\n').count(), 1);
    }

    #[test]
    fn read_since_filters_by_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("log.jsonl");
        let mut f = open_or_create_log(&log_path).unwrap();

        let m1 = make_msg("01HZA00000000000000000000A", "first");
        let m2 = make_msg("01HZB00000000000000000000B", "second");
        let m3 = make_msg("01HZC00000000000000000000C", "third");
        append(&mut f, &m1).unwrap();
        append(&mut f, &m2).unwrap();
        append(&mut f, &m3).unwrap();

        let after_m1 = read_since(&mut f, Some(&m1.id)).unwrap();
        assert_eq!(after_m1.len(), 2);
        assert_eq!(after_m1[0].body, "second");
        assert_eq!(after_m1[1].body, "third");

        let after_m3 = read_since(&mut f, Some(&m3.id)).unwrap();
        assert!(after_m3.is_empty(), "cursor at tail → no unread");
    }

    #[test]
    fn read_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("log.jsonl");

        // Write a malformed line first, then a valid one.
        {
            let mut raw = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&log_path)
                .unwrap();
            raw.write_all(b"this is not json\n").unwrap();
            raw.write_all(b"{\"v\":1,\"id\":\"01HZA8K9F0RS3JXG7QZ4N5VTBC\",\"author\":\"x\",\"ts\":1,\"body\":\"ok\"}\n").unwrap();
        }

        let mut f = open_or_create_log(&log_path).unwrap();
        let read = read_since(&mut f, None).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].body, "ok");
    }

    #[test]
    fn empty_log_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("log.jsonl");
        let mut f = open_or_create_log(&log_path).unwrap();
        let read = read_since(&mut f, None).unwrap();
        assert!(read.is_empty());
    }

    /// Concurrent writers MUST NOT corrupt each other's lines.
    /// Each thread opens its own File handle; fcntl OFD locks serialise.
    #[test]
    fn concurrent_writers_serialise_via_fcntl() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = Arc::new(dir.path().join("log.jsonl"));
        // Pre-create so there's no parent-dir-create race in test.
        drop(open_or_create_log(&log_path).unwrap());

        const WRITERS: usize = 4;
        const PER_WRITER: usize = 25;

        let mut handles = Vec::new();
        for w in 0..WRITERS {
            let path = Arc::clone(&log_path);
            handles.push(std::thread::spawn(move || {
                let mut f = open_or_create_log(&path).unwrap();
                for i in 0..PER_WRITER {
                    let id = format!("01HZ{:02}{:020}", w, i);
                    let body = format!("writer {w} message {i}");
                    let msg = make_msg(&id, &body);
                    append(&mut f, &msg).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Every line MUST parse as a Message — no torn writes.
        let mut f = open_or_create_log(&log_path).unwrap();
        let read = read_since(&mut f, None).unwrap();
        assert_eq!(
            read.len(),
            WRITERS * PER_WRITER,
            "all {} appends MUST be intact and parseable",
            WRITERS * PER_WRITER
        );

        // No two messages should share an id.
        let mut ids: Vec<&str> = read.iter().map(|m| m.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), WRITERS * PER_WRITER, "no duplicate ids");
    }
}
