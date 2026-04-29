//! `cc-connect room {start,join}` — the room launcher.
//!
//! Decides at runtime how to render the room: a multiplexer-managed
//! layout (zellij preferred, tmux fallback) with claude L + cc-chat-ui R,
//! or — if neither multiplexer is installed — exec into the legacy
//! `cc-connect-tui` binary which embeds Claude itself in a single window.
//!
//! Flow:
//!   1. setup wizard (hook + MCP + nick + relay choice — same prompts the
//!      old TUI shimmed)
//!   2. for `start`: spawn `host-bg start` to get a ticket; for `join`:
//!      caller already gave us a ticket
//!   3. detect multiplexer
//!   4. if zellij/tmux: ensure chat-daemon is running for the topic, then
//!      exec the multiplexer with CC_CONNECT_ROOM in the env so claude's
//!      hook + chat-ui both see it
//!   5. else: exec cc-connect-tui (which spawns its own in-process
//!      chat_session — DON'T also start chat-daemon, the two would race
//!      on chat.sock and the gossip identity)

use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::ticket_payload::TicketPayload;
use cc_connect_core::ticket::decode_room_code;

const ZELLIJ_LAYOUT: &str = include_str!("../../../layouts/cc-connect.kdl");
const TMUX_LAUNCHER: &str = include_str!("../../../layouts/cc-connect.tmux.sh");
const CLAUDE_WRAPPER_SH: &str = include_str!("../../../layouts/claude-wrap.sh");
const AUTO_REPLY_PROMPT: &str = include_str!("../../../layouts/auto-reply-prompt.md");

#[derive(Debug, Clone, Copy)]
enum Multiplexer {
    Zellij,
    Tmux,
    /// Neither found — fallback to embedded TUI.
    None,
}

pub fn run_start(relay: Option<&str>, nick: Option<&str>, claude_args: &[String]) -> Result<()> {
    setup_wizard(nick)?;
    let resolved_relay = crate::setup::ensure_relay_choice(relay).unwrap_or_else(|e| {
        eprintln!("(setup: relay prompt failed: {e:#}; defaulting to n0)");
        None
    });

    let ticket =
        spawn_host_bg(resolved_relay.as_deref()).context("spawn host-bg start to mint a ticket")?;
    println!("[room] daemon started, joining…");

    launch_room(
        &ticket,
        resolved_relay.as_deref(),
        claude_args,
        /* hosting */ true,
    )
}

pub fn run_join(
    ticket: &str,
    relay: Option<&str>,
    nick: Option<&str>,
    claude_args: &[String],
) -> Result<()> {
    setup_wizard(nick)?;
    launch_room(ticket, relay, claude_args, /* hosting */ false)
}

// ---- shared launcher ----------------------------------------------------

fn launch_room(
    ticket: &str,
    relay: Option<&str>,
    claude_args: &[String],
    hosting: bool,
) -> Result<()> {
    let mux = detect_multiplexer();
    match mux {
        Multiplexer::Zellij | Multiplexer::Tmux => {
            // Multiplexer path: chat-daemon owns the chat substrate, cc-chat-ui
            // attaches to it from one pane while claude runs in the other.
            let topic_hex = decode_topic_hex(ticket)?;
            ensure_chat_daemon(ticket, /* no_relay */ false, relay)?;
            match mux {
                Multiplexer::Zellij => exec_zellij(&topic_hex, claude_args),
                Multiplexer::Tmux => exec_tmux(&topic_hex, claude_args),
                Multiplexer::None => unreachable!(),
            }
        }
        Multiplexer::None => {
            // Fallback: cc-connect-tui spawns its own chat_session in-process.
            // Don't start chat-daemon — both binding chat.sock for the same
            // topic would conflict, and the gossip mesh would see the same
            // identity from two processes.
            eprintln!("! note: zellij and tmux not found — falling back to embedded TUI.");
            eprintln!("! install one of:");
            eprintln!("!   brew install zellij    # macOS (recommended)");
            eprintln!("!   apt install tmux       # debian/ubuntu");
            eprintln!("! …for the multi-pane chat-ui experience.");
            exec_tui_fallback(ticket, relay, claude_args, hosting)
        }
    }
}

// ---- setup wizard wrapper ----------------------------------------------

fn setup_wizard(nick: Option<&str>) -> Result<()> {
    if let Err(e) = crate::setup::ensure_hook_installed() {
        eprintln!("(setup: hook check failed: {e:#})");
    }
    if let Err(e) = crate::setup::ensure_mcp_installed() {
        eprintln!("(setup: mcp install failed: {e:#})");
    }
    if let Err(e) = crate::setup::ensure_self_nick(nick) {
        eprintln!("(setup: nick prompt failed: {e:#})");
    }
    Ok(())
}

// ---- host-bg ticket capture --------------------------------------------

/// Spawn `cc-connect host-bg start [--relay <url>]`, parse the printed
/// ticket out of stdout. The host-bg daemon stays alive detached.
fn spawn_host_bg(relay: Option<&str>) -> Result<String> {
    let cc_connect = locate_self_bin()?;
    let mut cmd = Command::new(&cc_connect);
    cmd.arg("host-bg").arg("start");
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawn {}", cc_connect.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "host-bg start failed (exit {:?}): {stderr}",
            out.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find_map(|l| {
            l.split_whitespace()
                .find(|w| w.starts_with("cc1-"))
                .map(|s| s.to_string())
        })
        .ok_or_else(|| anyhow!("host-bg start did not print a ticket; output was:\n{stdout}"))
}

// ---- chat-daemon idempotent start --------------------------------------

/// Ensure the chat-daemon is running for `ticket`. `chat_daemon::run_start`
/// is itself idempotent (prints `ALREADY <topic> <pid>` if a live daemon
/// owns the same topic), so we just shell out to ourselves and let it
/// figure it out.
fn ensure_chat_daemon(ticket: &str, no_relay: bool, relay: Option<&str>) -> Result<()> {
    let cc_connect = locate_self_bin()?;
    let mut cmd = Command::new(&cc_connect);
    cmd.arg("chat-daemon").arg("start").arg(ticket);
    if no_relay {
        cmd.arg("--no-relay");
    }
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawn {} chat-daemon start", cc_connect.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Err(anyhow!(
            "chat-daemon start failed (exit {:?}):\nstdout:{stdout}\nstderr:{stderr}",
            out.status.code()
        ));
    }
    // We don't care whether the first line was "READY <topic>" or
    // "ALREADY <topic> <pid>" — both mean a live daemon is bound to the
    // chat.sock for this topic.
    Ok(())
}

// ---- multiplexer detection + exec --------------------------------------

fn detect_multiplexer() -> Multiplexer {
    if which("zellij").is_some() {
        return Multiplexer::Zellij;
    }
    if which("tmux").is_some() {
        return Multiplexer::Tmux;
    }
    Multiplexer::None
}

fn exec_zellij(topic_hex: &str, _claude_args: &[String]) -> Result<()> {
    // Materialise the layout to a tmpfile so zellij --layout can pick it
    // up. Two substitutions:
    //   __CC_CHAT_UI_BIN__   → absolute path to cc-chat-ui next to our own
    //                          binary (without this zellij does a bare PATH
    //                          lookup and panics if the binary isn't there).
    //   __CLAUDE_WRAPPER__   → absolute path to the claude-wrap.sh shim
    //                          (which prepends --append-system-prompt with
    //                          the auto-reply directive when present).
    let chat_ui_bin = locate_chat_ui_bin()?;
    let claude_wrapper = prepare_claude_wrapper()?;
    let auto_reply = prepare_auto_reply_prompt()?;

    let layout_kdl = ZELLIJ_LAYOUT
        .replace("__CC_CHAT_UI_BIN__", &chat_ui_bin.to_string_lossy())
        .replace("__CLAUDE_WRAPPER__", &claude_wrapper.to_string_lossy());
    let layout_path = write_tmp_layout("cc-connect", "kdl", &layout_kdl)?;
    let session_short = topic_hex
        .chars()
        .take(12.min(topic_hex.len()))
        .collect::<String>();

    // Use -n / --new-session-with-layout (NOT --layout + --session): with
    // --session set, plain --layout treats the layout as "add tabs to that
    // session" and errors with "There is no active session!" when the session
    // doesn't exist yet. -n forces "create new session with this layout"
    // unconditionally.
    let mut cmd = Command::new("zellij");
    cmd.arg("--session")
        .arg(format!("cc-connect-{session_short}"));
    cmd.arg("-n").arg(&layout_path);
    cmd.env("CC_CONNECT_ROOM", topic_hex);
    if let Some(p) = auto_reply {
        cmd.env("CC_CONNECT_AUTO_REPLY_FILE", &p);
    }

    // claude_args (e.g. `--model opus`) go into the embedded claude. We
    // can't push them through zellij's KDL `args` from the Rust side
    // without sed-replacing the layout file — for v1, expose them via an
    // env var the user can plumb into a custom layout if they want.
    // Most users won't pass claude args to `room start` anyway.

    let err = cmd.exec();
    Err(anyhow!("exec zellij failed: {err}"))
}

fn exec_tmux(topic_hex: &str, _claude_args: &[String]) -> Result<()> {
    let claude_wrapper = prepare_claude_wrapper()?;
    let auto_reply = prepare_auto_reply_prompt()?;

    let tmux_script =
        TMUX_LAUNCHER.replace("__CLAUDE_WRAPPER__", &claude_wrapper.to_string_lossy());
    let script_path = write_tmp_layout("cc-connect", "tmux.sh", &tmux_script)?;
    // Make it executable.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700));

    let mut cmd = Command::new("bash");
    cmd.arg(&script_path);
    cmd.env("CC_CONNECT_ROOM", topic_hex);
    if let Some(p) = auto_reply {
        cmd.env("CC_CONNECT_AUTO_REPLY_FILE", &p);
    }
    let err = cmd.exec();
    Err(anyhow!("exec tmux launcher failed: {err}"))
}

/// Per-UID temp directory used for ephemeral, machine-local state shared
/// between the cc-connect launcher, the claude wrapper script, and the
/// chat-ui pane. Mirrors the `/tmp/cc-connect-$UID/` convention used by
/// `chat_session::pid_file_path` for active-rooms PID files.
fn cc_connect_tmp_dir() -> PathBuf {
    let uid = rustix::process::geteuid().as_raw();
    std::env::temp_dir().join(format!("cc-connect-{uid}"))
}

/// Write `claude-wrap.sh` into `/tmp/cc-connect-$UID/`, chmod 0700,
/// idempotent on every call. The wrapper is the actual binary spawned by
/// the multiplexer; it picks up `--append-system-prompt` from the
/// auto-reply file if one is present, else exec's plain claude.
fn prepare_claude_wrapper() -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = cc_connect_tmp_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create_dir_all {}", dir.display()))?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let path = dir.join("claude-wrap.sh");
    std::fs::write(&path, CLAUDE_WRAPPER_SH)
        .with_context(|| format!("write {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
    Ok(path)
}

/// Write the auto-reply system-prompt directive into
/// `/tmp/cc-connect-$UID/auto-reply.md` and return the path. Returns
/// `Ok(None)` if the user has opted out via `CC_CONNECT_NO_AUTO_REPLY=1`
/// — the wrapper script then falls through to plain claude.
fn prepare_auto_reply_prompt() -> Result<Option<PathBuf>> {
    if std::env::var_os("CC_CONNECT_NO_AUTO_REPLY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Ok(None);
    }
    use std::os::unix::fs::PermissionsExt;
    let dir = cc_connect_tmp_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create_dir_all {}", dir.display()))?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let path = dir.join("auto-reply.md");
    std::fs::write(&path, AUTO_REPLY_PROMPT)
        .with_context(|| format!("write {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok(Some(path))
}

fn exec_tui_fallback(
    ticket: &str,
    relay: Option<&str>,
    claude_args: &[String],
    hosting: bool,
) -> Result<()> {
    let tui = locate_tui_bin()?;
    let mut cmd = Command::new(&tui);
    if hosting {
        // `cc-connect-tui start` starts its own host-bg internally — but
        // we already started one in run_start. To avoid spawning two
        // host-bgs for the same room, we always go through `join` with
        // the ticket we already have.
    }
    cmd.arg("join").arg(ticket);
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    if !claude_args.is_empty() {
        cmd.arg("--");
        cmd.args(claude_args);
    }
    let err = cmd.exec();
    Err(anyhow!("exec {} failed: {err}", tui.display()))
}

// ---- helpers -----------------------------------------------------------

fn locate_self_bin() -> Result<PathBuf> {
    std::env::current_exe().context("current_exe")
}

fn locate_tui_bin() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("current_exe has no parent: {}", exe.display()))?;
    let tui = dir.join("cc-connect-tui");
    if !tui.exists() {
        bail!(
            "fallback to cc-connect-tui requested but {} not found — \
             install zellij or tmux, or run `cargo build --workspace --release`",
            tui.display()
        );
    }
    Ok(tui)
}

fn locate_chat_ui_bin() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("current_exe has no parent: {}", exe.display()))?;
    let bin = dir.join("cc-chat-ui");
    if bin.exists() {
        return Ok(bin);
    }
    // Fallback: PATH lookup, in case the user installed the binary system-wide.
    if let Some(p) = which("cc-chat-ui") {
        return Ok(p);
    }
    bail!(
        "cc-chat-ui binary not found at {} or on $PATH — \
         run `cd chat-ui && bun run build` or `./install.sh`",
        bin.display()
    )
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn write_tmp_layout(stem: &str, ext: &str, contents: &str) -> Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    let dir = std::env::temp_dir();
    let path = dir.join(format!("{stem}-{pid}-{ms}.{ext}"));
    let mut f = std::fs::File::create(&path)
        .with_context(|| format!("create tmp layout {}", path.display()))?;
    f.write_all(contents.as_bytes())
        .with_context(|| format!("write tmp layout {}", path.display()))?;
    Ok(path)
}

fn decode_topic_hex(ticket: &str) -> Result<String> {
    let bytes =
        decode_room_code(ticket).with_context(|| format!("decode room code: {:.20}…", ticket))?;
    let payload = TicketPayload::from_bytes(&bytes)?;
    let mut out = String::with_capacity(64);
    for b in payload.topic.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}
