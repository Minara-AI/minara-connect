//! Session state — per-Claude-Code-process Room membership and pending Joins.
//!
//! See `PROTOCOL.md` §7.3 step 0 for the trust boundary this module
//! implements, and `SECURITY.md` "Cross-process Claude isolation" for the
//! threat model. Replaces the pre-v0.6 `CC_CONNECT_ROOM` env-var gate
//! (PROTOCOL.md change-log entry under §7.3).
//!
//! ## Rooms file
//!
//! Path: `~/.cc-connect/sessions/by-claude-pid/<claude_pid>/rooms.json`
//! Mode: `0600` file, parent directories `0700`.
//! Content (canonical v1):
//!
//! ```json
//! { "v": 1, "topics": ["<topic_hex>", "<topic_hex>", ...] }
//! ```
//!
//! Lifetime: each entry is owned by exactly one running Claude Code
//! process. When that process exits, the file is orphaned; the next
//! `cc-connect-hook` or `cc-connect-mcp` startup calls
//! [`prune_dead_pid_sessions`] which removes orphaned `<pid>/` dirs.
//!
//! ## Pending-joins (consent gate)
//!
//! Path: `~/.cc-connect/pending-joins/<token>.json`
//! Mode: `0600` file, parent directory `0700`.
//! Content:
//!
//! ```json
//! {
//!   "v": 1,
//!   "token": "<random hex>",
//!   "claude_pid": <u32>,
//!   "topic": "<topic_hex>",
//!   "ticket": "<cc1-...>",
//!   "requested_at_ms": <u64 unix ms>
//! }
//! ```
//!
//! `cc_join_room` writes one of these and returns `token`. The human
//! reviews the pending join in the side-channel viewer (CLI watch /
//! VSCode chat panel), then runs `cc-connect accept <token>` (or clicks
//! Accept), which atomically reads-and-deletes the pending file via
//! [`consume_pending_join`] and adds the topic to the rooms file via
//! [`add_topic`]. Until that explicit consent, the hook never injects
//! the topic.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Schema version for both `rooms.json` and pending-join files. Bump on
/// any breaking field change; readers MUST refuse versions they don't
/// understand to avoid silent data loss.
const STATE_VERSION: u64 = 1;

/// Random bytes per pending-join token. 16 → 32 hex chars; not a
/// cryptographic secret (the consent flow is local), just a non-guessable
/// filename so two concurrent Joins don't collide.
const TOKEN_BYTES: usize = 16;

// =============================================================================
// Rooms file: ~/.cc-connect/sessions/by-claude-pid/<pid>/rooms.json
// =============================================================================

#[derive(Debug, Serialize, Deserialize)]
struct RoomsFile {
    v: u64,
    topics: Vec<String>,
}

/// Absolute path to the rooms file for the given Claude Code PID.
pub fn rooms_path(claude_pid: u32) -> Result<PathBuf> {
    Ok(sessions_root()?
        .join("by-claude-pid")
        .join(claude_pid.to_string())
        .join("rooms.json"))
}

/// Read the topic list this Claude Code process has joined. Returns
/// `Ok(vec![])` for missing or empty files (the no-rooms case the hook
/// must treat as "no-op").
pub fn list_topics(claude_pid: u32) -> Result<Vec<String>> {
    let path = rooms_path(claude_pid)?;
    match read_rooms_file(&path) {
        Ok(rooms) => Ok(rooms.topics),
        Err(e) if is_not_found(&e) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Add `topic_hex` to this Claude Code process's rooms. Idempotent — a
/// duplicate add is a no-op. Creates parent directories with mode `0700`
/// and the file itself with mode `0600`.
pub fn add_topic(claude_pid: u32, topic_hex: &str) -> Result<()> {
    let path = rooms_path(claude_pid)?;
    ensure_parent_dirs(&path)?;
    let mut rooms = read_rooms_file(&path).unwrap_or_else(|_| RoomsFile {
        v: STATE_VERSION,
        topics: Vec::new(),
    });
    if !rooms.topics.iter().any(|t| t == topic_hex) {
        rooms.topics.push(topic_hex.to_string());
    }
    write_rooms_file(&path, &rooms)
}

/// Remove `topic_hex` from this Claude Code process's rooms. Idempotent
/// — removing a topic the session never joined is a no-op. **Does NOT**
/// stop the underlying chat-daemon (other sessions may still be using
/// the room); see ADR-0006 for the reference-counting non-decision.
pub fn remove_topic(claude_pid: u32, topic_hex: &str) -> Result<()> {
    let path = rooms_path(claude_pid)?;
    let mut rooms = match read_rooms_file(&path) {
        Ok(r) => r,
        Err(e) if is_not_found(&e) => return Ok(()),
        Err(e) => return Err(e),
    };
    let before = rooms.topics.len();
    rooms.topics.retain(|t| t != topic_hex);
    if rooms.topics.len() != before {
        write_rooms_file(&path, &rooms)?;
    }
    Ok(())
}

/// Remove every topic from this Claude Code process's rooms. Used by
/// `cc_leave_room()` when called without a `topic` argument.
pub fn remove_all_topics(claude_pid: u32) -> Result<()> {
    let path = rooms_path(claude_pid)?;
    if !path.exists() {
        return Ok(());
    }
    let rooms = RoomsFile {
        v: STATE_VERSION,
        topics: Vec::new(),
    };
    write_rooms_file(&path, &rooms)
}

/// Sweep `~/.cc-connect/sessions/by-claude-pid/` and remove every
/// `<pid>/` subdirectory whose PID is not currently alive. Returns the
/// count of pruned entries.
///
/// Called opportunistically on every `cc-connect-hook` invocation
/// (PROTOCOL.md §7.3 step 0a) and at MCP server startup. Cheap: typical
/// directories hold ≤ a handful of entries.
pub fn prune_dead_pid_sessions() -> Result<usize> {
    let root = match sessions_root() {
        Ok(p) => p,
        Err(_) => return Ok(0),
    };
    let by_pid = root.join("by-claude-pid");
    if !by_pid.exists() {
        return Ok(0);
    }
    let entries =
        std::fs::read_dir(&by_pid).with_context(|| format!("read_dir {}", by_pid.display()))?;
    let mut pruned = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid_str) = name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if !pid_alive(pid)? {
            // Best-effort: if removal fails (e.g. a concurrent hook is
            // mid-write), leave it for the next prune cycle.
            if std::fs::remove_dir_all(entry.path()).is_ok() {
                pruned += 1;
            }
        }
    }
    Ok(pruned)
}

// =============================================================================
// Pending-joins: ~/.cc-connect/pending-joins/<token>.json
// =============================================================================

/// One pending-join request awaiting human consent. Returned by
/// [`list_pending_joins`] (for the watch UI) and
/// [`consume_pending_join`] (after the human accepts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingJoin {
    /// Schema version — see [`STATE_VERSION`].
    pub v: u64,
    /// Random hex token, also the filename stem.
    pub token: String,
    /// PID of the Claude Code process that requested the join.
    pub claude_pid: u32,
    /// Topic hex the Claude wants to join.
    pub topic: String,
    /// Original ticket text — kept so `cc-connect accept` can spawn the
    /// chat-daemon if it isn't already running for this topic.
    pub ticket: String,
    /// `SystemTime::now()` at request time, milliseconds since UNIX epoch.
    pub requested_at_ms: u64,
}

/// Create a pending-join file and return its token. The token is also
/// returned to Claude as the `pending_token` field of the `cc_join_room`
/// MCP response so the human can reference it in `cc-connect accept`.
pub fn create_pending_join(claude_pid: u32, topic: &str, ticket: &str) -> Result<String> {
    let token = random_token()?;
    let pj = PendingJoin {
        v: STATE_VERSION,
        token: token.clone(),
        claude_pid,
        topic: topic.to_string(),
        ticket: ticket.to_string(),
        requested_at_ms: now_unix_millis(),
    };
    let path = pending_join_path(&token)?;
    ensure_parent_dirs(&path)?;
    write_pending_join(&path, &pj)?;
    Ok(token)
}

/// List every pending-join request currently awaiting human consent.
/// Returned in unspecified order; callers that need a stable order
/// should sort by `requested_at_ms`.
pub fn list_pending_joins() -> Result<Vec<PendingJoin>> {
    let dir = pending_joins_root()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        match read_pending_join(&path) {
            Ok(pj) => out.push(pj),
            Err(_) => {
                // Best-effort: skip malformed files. They'll get cleaned
                // up by `cc-connect uninstall --purge` or by a manual
                // `rm` after the user investigates.
                continue;
            }
        }
    }
    Ok(out)
}

/// Atomically read-and-delete a pending-join. The deletion is the gate:
/// only one caller can successfully consume; subsequent attempts return
/// the not-found error. Used by `cc-connect accept <token>` and by the
/// VSCode panel's Accept button.
pub fn consume_pending_join(token: &str) -> Result<PendingJoin> {
    let path = pending_join_path(token)?;
    let pj = read_pending_join(&path)?;
    // Order matters: rename out of the way before deleting, so a racing
    // reader sees either the original or nothing — never half-deleted.
    let staging = path.with_extension("json.consumed");
    std::fs::rename(&path, &staging)
        .with_context(|| format!("rename {} → {}", path.display(), staging.display()))?;
    std::fs::remove_file(&staging).ok();
    Ok(pj)
}

// =============================================================================
// On-disk helpers
// =============================================================================

fn sessions_root() -> Result<PathBuf> {
    Ok(home_dir()?.join(".cc-connect").join("sessions"))
}

fn pending_joins_root() -> Result<PathBuf> {
    Ok(home_dir()?.join(".cc-connect").join("pending-joins"))
}

fn pending_join_path(token: &str) -> Result<PathBuf> {
    if token.is_empty() || token.contains('/') || token.contains('\\') || token.contains('\0') {
        bail!("PENDING_TOKEN_INVALID: rejecting token `{token}`");
    }
    Ok(pending_joins_root()?.join(format!("{token}.json")))
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME env var not set"))
}

fn ensure_parent_dirs(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create_dir_all {}", parent.display()))?;
    // Walk every newly-created ancestor under `~/.cc-connect/` and tighten
    // it to `0700`. We can't simply `chmod` `parent` because intermediate
    // dirs (sessions/, sessions/by-claude-pid/) may also be brand-new.
    let cc_root = home_dir()?.join(".cc-connect");
    let mut cursor = parent.to_path_buf();
    while cursor.starts_with(&cc_root) && cursor != cc_root {
        if let Ok(meta) = std::fs::metadata(&cursor) {
            if meta.permissions().mode() & 0o777 != 0o700 {
                std::fs::set_permissions(&cursor, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        if !cursor.pop() {
            break;
        }
    }
    Ok(())
}

fn read_rooms_file(path: &Path) -> Result<RoomsFile> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let rooms: RoomsFile =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    if rooms.v != STATE_VERSION {
        bail!(
            "ROOMS_VERSION_MISMATCH: {} declares v={} but this binary only reads v={}",
            path.display(),
            rooms.v,
            STATE_VERSION
        );
    }
    Ok(rooms)
}

fn write_rooms_file(path: &Path, rooms: &RoomsFile) -> Result<()> {
    let body = serde_json::to_vec_pretty(rooms).context("serialize rooms.json")?;
    write_atomic_0600(path, &body)
}

fn read_pending_join(path: &Path) -> Result<PendingJoin> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)
        .with_context(|| format!("read {}", path.display()))?;
    let pj: PendingJoin =
        serde_json::from_str(&buf).with_context(|| format!("parse {}", path.display()))?;
    if pj.v != STATE_VERSION {
        bail!(
            "PENDING_VERSION_MISMATCH: {} declares v={} but this binary only reads v={}",
            path.display(),
            pj.v,
            STATE_VERSION
        );
    }
    Ok(pj)
}

fn write_pending_join(path: &Path, pj: &PendingJoin) -> Result<()> {
    let body = serde_json::to_vec_pretty(pj).context("serialize pending-join")?;
    write_atomic_0600(path, &body)
}

/// Write `bytes` to `path` atomically (tmp file → fsync → rename),
/// creating it with mode 0600.
fn write_atomic_0600(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent", path.display()))?;
    let tmp = unique_tmp_path(parent, path)?;
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("create tmp {}", tmp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("write tmp {}", tmp.display()))?;
        file.sync_all().context("fsync tmp")?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    if let Ok(parent_dir) = File::open(parent) {
        let _ = parent_dir.sync_all();
    }
    Ok(())
}

fn unique_tmp_path(parent: &Path, target: &Path) -> Result<PathBuf> {
    let base = target
        .file_name()
        .ok_or_else(|| anyhow!("target {} has no file_name", target.display()))?;
    let mut suffix = [0u8; 8];
    getrandom::getrandom(&mut suffix).map_err(|e| anyhow!("getrandom: {e}"))?;
    let suffix_hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let mut name = base.to_os_string();
    name.push(format!(".tmp.{suffix_hex}"));
    Ok(parent.join(name))
}

fn random_token() -> Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow!("getrandom: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn pid_alive(pid: u32) -> Result<bool> {
    let pid_obj = match rustix::process::Pid::from_raw(pid as i32) {
        Some(p) => p,
        None => return Ok(false),
    };
    match rustix::process::test_kill_process(pid_obj) {
        Ok(()) => Ok(true),
        // ESRCH is the only "definitely dead" verdict.
        Err(e) if e == rustix::io::Errno::SRCH => Ok(false),
        // EPERM means the process exists but we can't signal it (different
        // UID, e.g. PID 1 = launchd on macOS). Treat as alive — pruning a
        // PID we can't even see would be incorrect, and these PIDs aren't
        // ones we'd have written ourselves anyway.
        Err(e) if e == rustix::io::Errno::PERM => Ok(true),
        Err(e) => Err(anyhow!("test_kill_process({pid}): {e}")),
    }
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|e| e.kind() == ErrorKind::NotFound)
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test isolation: redirect `$HOME` to a temp dir for the duration of
    /// each test. (Tests are serial — see `#[serial]` in cargo manifest if
    /// concurrent runs ever break this.)
    fn with_temp_home<R>(f: impl FnOnce(&Path) -> R) -> R {
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        // SAFETY: tests in this module are run serially via single-threaded
        // mutex below; no concurrent reader sees a partial swap.
        std::env::set_var("HOME", dir.path());
        let result = f(dir.path());
        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
        result
    }

    /// Cargo's default test runner is multi-threaded; HOME mutation must
    /// be serialised. Each test acquires this mutex first.
    fn home_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn list_topics_missing_returns_empty() {
        let _guard = home_lock();
        with_temp_home(|_| {
            let topics = list_topics(5_000_000).unwrap();
            assert!(topics.is_empty());
        });
    }

    #[test]
    fn add_topic_then_list() {
        let _guard = home_lock();
        with_temp_home(|home| {
            add_topic(12345, "abc123").unwrap();
            let topics = list_topics(12345).unwrap();
            assert_eq!(topics, vec!["abc123".to_string()]);
            // Mode check on the file.
            let path = home
                .join(".cc-connect")
                .join("sessions")
                .join("by-claude-pid")
                .join("12345")
                .join("rooms.json");
            assert!(path.exists());
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "rooms.json must be mode 0600");
            // Parent dir mode.
            let parent_mode = std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(parent_mode, 0o700, "by-claude-pid/<pid>/ must be mode 0700");
        });
    }

    #[test]
    fn add_topic_idempotent() {
        let _guard = home_lock();
        with_temp_home(|_| {
            add_topic(54321, "topic_a").unwrap();
            add_topic(54321, "topic_a").unwrap();
            add_topic(54321, "topic_a").unwrap();
            let topics = list_topics(54321).unwrap();
            assert_eq!(topics, vec!["topic_a".to_string()]);
        });
    }

    #[test]
    fn add_topic_preserves_order() {
        let _guard = home_lock();
        with_temp_home(|_| {
            add_topic(11, "first").unwrap();
            add_topic(11, "second").unwrap();
            add_topic(11, "third").unwrap();
            assert_eq!(
                list_topics(11).unwrap(),
                vec![
                    "first".to_string(),
                    "second".to_string(),
                    "third".to_string()
                ]
            );
        });
    }

    #[test]
    fn remove_topic_only_removes_named() {
        let _guard = home_lock();
        with_temp_home(|_| {
            add_topic(7, "alpha").unwrap();
            add_topic(7, "beta").unwrap();
            add_topic(7, "gamma").unwrap();
            remove_topic(7, "beta").unwrap();
            assert_eq!(
                list_topics(7).unwrap(),
                vec!["alpha".to_string(), "gamma".to_string()]
            );
        });
    }

    #[test]
    fn remove_topic_idempotent_for_unknown() {
        let _guard = home_lock();
        with_temp_home(|_| {
            remove_topic(1, "never-added").unwrap();
            add_topic(1, "real").unwrap();
            remove_topic(1, "never-added").unwrap();
            assert_eq!(list_topics(1).unwrap(), vec!["real".to_string()]);
        });
    }

    #[test]
    fn remove_all_topics_clears_list() {
        let _guard = home_lock();
        with_temp_home(|_| {
            add_topic(42, "x").unwrap();
            add_topic(42, "y").unwrap();
            remove_all_topics(42).unwrap();
            assert!(list_topics(42).unwrap().is_empty());
        });
    }

    #[test]
    fn version_mismatch_refused() {
        let _guard = home_lock();
        with_temp_home(|home| {
            let dir = home
                .join(".cc-connect")
                .join("sessions")
                .join("by-claude-pid")
                .join("99");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("rooms.json"), br#"{"v": 999, "topics": ["x"]}"#).unwrap();
            let err = list_topics(99).unwrap_err();
            assert!(
                err.to_string().contains("ROOMS_VERSION_MISMATCH"),
                "got: {err}"
            );
        });
    }

    #[test]
    fn prune_removes_dead_pid_dirs() {
        let _guard = home_lock();
        with_temp_home(|home| {
            // The test process itself is guaranteed alive AND owned by us
            // (so test_kill_process returns Ok rather than EPERM). Pair
            // it with PID 5_000_000 which is above pid_max on every
            // mainstream kernel, so it's guaranteed dead.
            let live_pid = std::process::id();
            let dead_pid = 5_000_000u32;
            add_topic(live_pid, "live-room").unwrap();
            add_topic(dead_pid, "dead-room").unwrap();
            let pruned = prune_dead_pid_sessions().unwrap();
            assert_eq!(pruned, 1, "exactly the dead PID should be pruned");
            // Live PID's entry remains.
            assert_eq!(
                list_topics(live_pid).unwrap(),
                vec!["live-room".to_string()]
            );
            // Dead PID's dir is gone.
            let dead_dir = home
                .join(".cc-connect")
                .join("sessions")
                .join("by-claude-pid")
                .join(dead_pid.to_string());
            assert!(!dead_dir.exists(), "dead PID dir should be removed");
        });
    }

    #[test]
    fn prune_skips_non_numeric_entries() {
        let _guard = home_lock();
        with_temp_home(|home| {
            let by_pid = home
                .join(".cc-connect")
                .join("sessions")
                .join("by-claude-pid");
            std::fs::create_dir_all(&by_pid).unwrap();
            std::fs::write(by_pid.join("README"), b"not a pid").unwrap();
            // Should not error or remove non-numeric entries.
            let _ = prune_dead_pid_sessions().unwrap();
            assert!(by_pid.join("README").exists());
        });
    }

    #[test]
    fn pending_join_create_then_consume() {
        let _guard = home_lock();
        with_temp_home(|_| {
            let token = create_pending_join(8888, "topichex", "cc1-fakeNicket").unwrap();
            assert_eq!(token.len(), TOKEN_BYTES * 2);
            // Listing returns the entry.
            let listed = list_pending_joins().unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].token, token);
            assert_eq!(listed[0].claude_pid, 8888);
            assert_eq!(listed[0].topic, "topichex");

            let pj = consume_pending_join(&token).unwrap();
            assert_eq!(pj.token, token);
            assert_eq!(pj.topic, "topichex");
            // Second consume must fail.
            let err = consume_pending_join(&token).unwrap_err();
            assert!(err.to_string().contains("No such file") || err.to_string().contains("open"));
            // Listing is now empty.
            assert!(list_pending_joins().unwrap().is_empty());
        });
    }

    #[test]
    fn pending_join_rejects_path_traversal_token() {
        let _guard = home_lock();
        with_temp_home(|_| {
            // The /-bearing token must be refused so a malicious caller
            // can't escape `pending-joins/`.
            assert!(consume_pending_join("../etc/passwd").is_err());
            assert!(consume_pending_join("foo/bar").is_err());
            assert!(consume_pending_join("").is_err());
        });
    }

    #[test]
    fn pending_join_unique_tokens() {
        let _guard = home_lock();
        with_temp_home(|_| {
            let mut seen = std::collections::HashSet::new();
            for _ in 0..32 {
                let token = create_pending_join(1, "t", "cc1-x").unwrap();
                assert!(seen.insert(token), "token collision (improbable)");
            }
        });
    }
}
