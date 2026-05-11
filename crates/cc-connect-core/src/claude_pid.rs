//! Claude PID Binding — find the Claude Code process that spawned this child.
//!
//! See `PROTOCOL.md` §7.3 step 0 (trust boundary) and `SECURITY.md`
//! "Cross-process Claude isolation" for the threat model. Replaces the
//! pre-v0.6 `CC_CONNECT_ROOM` env-var gate with a process-tree walk:
//! both `cc-connect-hook` and `cc-connect-mcp` are spawned as children
//! (direct or shell-wrapped) of a `claude` binary. They walk up the
//! parent chain until they hit a process whose executable basename is
//! `claude`; that PID becomes the key into
//! `~/.cc-connect/sessions/by-claude-pid/<pid>/rooms.json` (see
//! `session_state.rs`).
//!
//! Walk depth is bounded at `MAX_DEPTH` (16) to terminate cleanly when
//! a child has no `claude` ancestor — typical for unrelated `cc-connect-mcp`
//! invocations or fresh terminals. The depth limit is generous: empirically
//! both Bash and MCP children are ≤ 2 hops away from `claude`.
//!
//! Linux: `/proc/<pid>/stat` for ppid (PROTOCOL.md §11 procfs), readlink
//! `/proc/<pid>/exe` for executable path.
//! macOS: `ps -p <pid> -o ppid=,comm=` for both ppid and full path. The
//! comm column on Darwin is the full executable path (not the truncated
//! TASK_COMM_LEN basename Linux uses); both kernels deliver enough to
//! compute the basename and compare it to `claude`.

use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;

/// Maximum depth of the parent-chain walk. Empirically both `cc-connect-hook`
/// and `cc-connect-mcp` are ≤ 2 hops from `claude` (direct child, or via
/// `zsh -c` for shell-wrapped Bash hooks). 16 is far above any plausible
/// real-world chain and bounds runaway loops if `getppid()` somehow
/// degenerates.
const MAX_DEPTH: usize = 16;

/// The basename we're hunting for in the parent chain. Matches both
/// `~/.local/bin/claude` and the VSCode-bundled
/// `…/anthropic.claude-code-…/native-binary/claude` because we compare
/// `Path::file_name()`, not the full path.
const CLAUDE_BIN_NAME: &str = "claude";

/// Walk the parent process chain from `start` until we reach a process
/// whose executable basename is `claude`. Return that process's PID.
///
/// Returns `Err` if:
///   - the walk reaches `init` (PID 1) without finding `claude`,
///   - any individual `read_ppid` / `read_exe_basename` call fails (the
///     process may have exited, or `/proc` may be unreadable), or
///   - the walk exceeds `MAX_DEPTH` levels.
///
/// Callers that need a no-op fall-through (e.g. the hook, which must
/// always exit 0 per PROTOCOL.md §7.4) should treat any `Err` here as
/// "this Claude process is not in any cc-connect Room".
pub fn find_claude_ancestor(start: u32) -> Result<u32> {
    find_claude_ancestor_with(start, |pid| {
        let ppid = read_ppid(pid)?;
        let basename = read_exe_basename(pid)?;
        Ok((ppid, basename))
    })
}

/// Test seam for [`find_claude_ancestor`]. `lookup` returns `(ppid,
/// basename_of_pid)` for an arbitrary PID; integration code passes a
/// closure that hits the real OS, unit tests pass an in-memory map.
pub(crate) fn find_claude_ancestor_with<F>(start: u32, mut lookup: F) -> Result<u32>
where
    F: FnMut(u32) -> Result<(u32, String)>,
{
    let mut pid = start;
    for depth in 0..MAX_DEPTH {
        let (ppid, _self_basename) =
            lookup(pid).with_context(|| format!("lookup PID {pid} at depth {depth}"))?;

        if ppid <= 1 {
            bail!(
                "CLAUDE_PID_NOT_FOUND: walked up {depth} level(s) from PID {start} \
                 and reached init/kernel without finding a `claude` ancestor"
            );
        }

        let (_, parent_basename) =
            lookup(ppid).with_context(|| format!("lookup parent PID {ppid} at depth {depth}"))?;
        if parent_basename == CLAUDE_BIN_NAME {
            return Ok(ppid);
        }
        pid = ppid;
    }
    bail!(
        "CLAUDE_PID_WALK_TOO_DEEP: walked {MAX_DEPTH} levels from PID {start} \
         without finding `claude` — process tree is unexpectedly deep"
    );
}

/// Read the parent PID of `pid`. Linux: parse `/proc/<pid>/stat`. macOS:
/// shell out to `ps`.
#[cfg(target_os = "linux")]
fn read_ppid(pid: u32) -> Result<u32> {
    let path = format!("/proc/{pid}/stat");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
    parse_proc_stat_ppid(&raw)
        .ok_or_else(|| anyhow!("PROC_STAT_PARSE: failed to parse ppid from {path}"))
}

#[cfg(target_os = "macos")]
fn read_ppid(pid: u32) -> Result<u32> {
    let (ppid, _) = ps_lookup(pid)?;
    Ok(ppid)
}

/// Read the basename of `pid`'s executable. Linux: readlink
/// `/proc/<pid>/exe`. macOS: shell out to `ps`.
#[cfg(target_os = "linux")]
fn read_exe_basename(pid: u32) -> Result<String> {
    let path = format!("/proc/{pid}/exe");
    let target = std::fs::read_link(&path).with_context(|| format!("readlink {path}"))?;
    basename_of(&target).ok_or_else(|| anyhow!("PROC_EXE_NO_BASENAME: {}", target.display()))
}

#[cfg(target_os = "macos")]
fn read_exe_basename(pid: u32) -> Result<String> {
    let (_, comm) = ps_lookup(pid)?;
    basename_of(Path::new(&comm)).ok_or_else(|| anyhow!("PS_COMM_NO_BASENAME: {comm}"))
}

/// Linux `/proc/<pid>/stat` parser. Extracts the `ppid` field (the 4th
/// space-separated field after the parenthesised `comm`). The comm
/// itself may contain spaces and embedded `)` characters, so we anchor
/// on the *last* `)` rather than a naive split.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_stat_ppid(raw: &str) -> Option<u32> {
    // Format: `<pid> (<comm-may-have-spaces-and-parens>) <state> <ppid> ...`
    let last_close = raw.rfind(')')?;
    let after = &raw[last_close + 1..];
    let mut parts = after.split_whitespace();
    let _state = parts.next()?;
    let ppid_str = parts.next()?;
    ppid_str.parse::<u32>().ok()
}

/// macOS `ps -p <pid> -o ppid=,comm=` invocation. Returns `(ppid, comm)`.
/// `comm` on Darwin is the full executable path (not the basename).
#[cfg(target_os = "macos")]
fn ps_lookup(pid: u32) -> Result<(u32, String)> {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=,comm="])
        .output()
        .with_context(|| format!("invoke ps -p {pid}"))?;
    if !out.status.success() {
        bail!(
            "PS_NONZERO: ps -p {pid} exited {}",
            out.status.code().unwrap_or(-1)
        );
    }
    let line = std::str::from_utf8(&out.stdout)
        .context("ps output not UTF-8")?
        .trim();
    if line.is_empty() {
        bail!("PS_EMPTY: ps -p {pid} returned no row (process exited?)");
    }
    // `ps -o ppid=,comm=` emits leading whitespace before ppid. Split on
    // the first whitespace gap; the rest is the comm field (which may
    // contain spaces in the path, so we don't split it further).
    let trimmed = line.trim_start();
    let (ppid_str, rest) = trimmed
        .split_once(char::is_whitespace)
        .ok_or_else(|| anyhow!("PS_FORMAT: unexpected `{trimmed}`"))?;
    let ppid: u32 = ppid_str
        .parse()
        .map_err(|e| anyhow!("PS_PPID_PARSE: `{ppid_str}`: {e}"))?;
    Ok((ppid, rest.trim().to_string()))
}

/// Take `Path::file_name()` and return it as an owned `String`. Returns
/// `None` if `path` is empty or ends in `..`.
fn basename_of(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|os| os.to_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Build a `lookup` closure for `find_claude_ancestor_with` from a
    /// PID → (ppid, basename) map.
    fn map_lookup(map: HashMap<u32, (u32, String)>) -> impl FnMut(u32) -> Result<(u32, String)> {
        let map = Mutex::new(map);
        move |pid: u32| {
            map.lock()
                .unwrap()
                .get(&pid)
                .cloned()
                .ok_or_else(|| anyhow!("test map missing PID {pid}"))
        }
    }

    #[test]
    fn finds_claude_when_direct_parent() {
        // Mirrors the cc-connect-mcp case: MCP server (PID 100), parent is
        // claude (PID 200), grandparent is VSCode (PID 300).
        let mut map = HashMap::new();
        map.insert(100, (200, "cc-connect-mcp".into()));
        map.insert(200, (300, "claude".into()));
        map.insert(300, (1, "Code Helper".into()));
        let result = find_claude_ancestor_with(100, map_lookup(map)).unwrap();
        assert_eq!(result, 200);
    }

    #[test]
    fn finds_claude_through_intermediate_shell() {
        // Mirrors the Bash tool case: zsh wrapper (PID 100), parent is
        // claude (PID 200). One hop up.
        let mut map = HashMap::new();
        map.insert(100, (200, "zsh".into()));
        map.insert(200, (300, "claude".into()));
        map.insert(300, (1, "Code Helper".into()));
        let result = find_claude_ancestor_with(100, map_lookup(map)).unwrap();
        assert_eq!(result, 200);
    }

    #[test]
    fn finds_claude_through_two_intermediate_shells() {
        // Defensive: bash → zsh → claude.
        let mut map = HashMap::new();
        map.insert(100, (101, "bash".into()));
        map.insert(101, (200, "zsh".into()));
        map.insert(200, (300, "claude".into()));
        map.insert(300, (1, "init".into()));
        let result = find_claude_ancestor_with(100, map_lookup(map)).unwrap();
        assert_eq!(result, 200);
    }

    #[test]
    fn errors_when_walked_to_init() {
        // No claude in the chain: returns Err so the hook can no-op.
        let mut map = HashMap::new();
        map.insert(100, (200, "cc-connect-mcp".into()));
        map.insert(200, (1, "shell".into()));
        let err = find_claude_ancestor_with(100, map_lookup(map)).unwrap_err();
        assert!(
            err.to_string().contains("CLAUDE_PID_NOT_FOUND"),
            "expected CLAUDE_PID_NOT_FOUND, got: {err}"
        );
    }

    #[test]
    fn errors_when_walk_exceeds_max_depth() {
        // Synthetic infinite shell chain: every PID has parent = self+1
        // with basename "shell". The walk must exit cleanly at MAX_DEPTH
        // rather than running away. PIDs are kept well above 1 so we
        // never trip the init-branch first.
        let lookup = |pid: u32| -> Result<(u32, String)> { Ok((pid + 1, "shell".to_string())) };
        let err = find_claude_ancestor_with(100, lookup).unwrap_err();
        assert!(
            err.to_string().contains("CLAUDE_PID_WALK_TOO_DEEP"),
            "expected CLAUDE_PID_WALK_TOO_DEEP, got: {err}"
        );
    }

    #[test]
    fn errors_when_lookup_fails_mid_walk() {
        // Lookup returns Err for unmapped PIDs.
        let mut map = HashMap::new();
        map.insert(100, (200, "cc-connect-mcp".into()));
        // Deliberately omit 200.
        let err = find_claude_ancestor_with(100, map_lookup(map)).unwrap_err();
        assert!(err.to_string().contains("lookup parent PID 200"));
    }

    #[test]
    fn parse_proc_stat_handles_simple_comm() {
        // `1234 (cat) S 5678 ...`
        let raw = "1234 (cat) S 5678 1234 1234 0 -1 4194560 ...";
        assert_eq!(parse_proc_stat_ppid(raw), Some(5678));
    }

    #[test]
    fn parse_proc_stat_handles_comm_with_spaces() {
        // `1234 (Code Helper) S 5678 ...` — a real macOS-app-style comm.
        let raw = "1234 (Code Helper) S 5678 1234 1234 0 -1 4194560 ...";
        assert_eq!(parse_proc_stat_ppid(raw), Some(5678));
    }

    #[test]
    fn parse_proc_stat_handles_comm_with_embedded_parens() {
        // PROTOCOL.md §7.3 fragment: comm may contain `)` so split on
        // *last* `)`. `(weird)name)` is valid.
        let raw = "9999 (weird)name) R 4242 9999 9999 0 -1 4194560 ...";
        assert_eq!(parse_proc_stat_ppid(raw), Some(4242));
    }

    #[test]
    fn parse_proc_stat_returns_none_on_garbage() {
        assert_eq!(parse_proc_stat_ppid("no-parens-here"), None);
        assert_eq!(parse_proc_stat_ppid("()"), None);
        assert_eq!(parse_proc_stat_ppid("(comm) S not-a-number"), None);
    }

    #[test]
    fn basename_of_extracts_filename() {
        assert_eq!(
            basename_of(Path::new("/usr/local/bin/claude")),
            Some("claude".to_string())
        );
        assert_eq!(
            basename_of(Path::new(
                "/Users/me/.vscode/extensions/anthropic.claude-code-2.1.132/native-binary/claude"
            )),
            Some("claude".to_string())
        );
        assert_eq!(basename_of(Path::new("claude")), Some("claude".to_string()));
    }

    /// Smoke test against the real OS: walking from the test process must
    /// either find a `claude` ancestor (when run inside Claude Code's test
    /// runner) or return `CLAUDE_PID_NOT_FOUND`. Either is acceptable; we
    /// just verify the call doesn't panic and produces a sensible result.
    #[test]
    fn live_walk_from_self_terminates() {
        let pid = std::process::id();
        match find_claude_ancestor(pid) {
            Ok(claude_pid) => {
                assert!(claude_pid > 1, "claude PID must be > 1, got {claude_pid}");
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("CLAUDE_PID_NOT_FOUND")
                        || msg.contains("CLAUDE_PID_WALK_TOO_DEEP")
                        || msg.contains("lookup"),
                    "unexpected error variant: {msg}"
                );
            }
        }
    }
}
