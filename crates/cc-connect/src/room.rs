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
    // Run doctor first so the user always sees their cc-connect /
    // cc-connect-hook / cc-connect-mcp binary paths + build ages. The
    // most common "I rebuilt but it didn't take effect" failure mode is
    // the user installing from one clone but invoking through a PATH
    // symlink that resolves to a different (older) one — surfacing the
    // mtimes makes that staleness obvious at a glance. Doctor's Err is
    // informational, never blocking.
    println!("[setup] running doctor...");
    if let Err(e) = crate::doctor::run() {
        eprintln!("(setup: doctor reported FAIL: {e:#})");
    }
    println!();

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

/// Single zellij session shared across every cc-connect room. First
/// room creates the session; subsequent rooms add a new tab via
/// `zellij action new-tab`.
const ZELLIJ_SESSION: &str = "cc-connect";

fn exec_zellij(topic_hex: &str, _claude_args: &[String]) -> Result<()> {
    // Resolve the four substitutions baked into the layout per launch:
    //   __CC_CHAT_UI_BIN__   chat-ui binary next to our own
    //   __CLAUDE_WRAPPER__   claude-wrap.sh shim (sets CC_CONNECT_ROOM
    //                        + prepends --append-system-prompt)
    //   __CC_CONNECT_ROOM__  topic hex (passed to wrapper as argv and
    //                        to chat-ui via --topic)
    //   __TAB_NAME__         12-char topic prefix shown in the tab bar
    let chat_ui_bin = locate_chat_ui_bin()?;
    let claude_wrapper = prepare_claude_wrapper()?;
    let _ = prepare_auto_reply_prompt()?; // wrapper reads it from /tmp default path
    let tab_name: String = topic_hex.chars().take(12.min(topic_hex.len())).collect();

    let layout_kdl = ZELLIJ_LAYOUT
        .replace("__CC_CHAT_UI_BIN__", &chat_ui_bin.to_string_lossy())
        .replace("__CLAUDE_WRAPPER__", &claude_wrapper.to_string_lossy())
        .replace("__CC_CONNECT_ROOM__", topic_hex)
        .replace("__TAB_NAME__", &tab_name);
    let layout_path = write_tmp_layout(&format!("cc-connect-{tab_name}"), "kdl", &layout_kdl)?;

    let inside_zellij = std::env::var_os("ZELLIJ").is_some();
    let session_state = zellij_session_state(ZELLIJ_SESSION);

    // EXITED stays in `list-sessions` until manually deleted; trying to
    // `action new-tab` against it fails with "There is no active
    // session!". Wipe it so the fresh-session path can recreate cleanly.
    if matches!(session_state, ZellijSessionState::Exited) {
        zellij_delete_session(ZELLIJ_SESSION);
    }
    let session_running = matches!(session_state, ZellijSessionState::Live);

    print_exit_hint();

    if session_running {
        // Add this room as a new tab in the existing session. Always
        // pass --session so we target cc-connect specifically, even
        // when the caller is inside a different zellij session.
        let status = Command::new("zellij")
            .args(["--session", ZELLIJ_SESSION, "action", "new-tab"])
            .args(["--layout", &layout_path.to_string_lossy()])
            .args(["--name", &tab_name])
            .status()
            .context("spawn zellij action new-tab")?;
        if !status.success() {
            // Race: session was killed (or had transitioned to EXITED in
            // the gap between our check and `action new-tab`). Wipe it
            // and fall through to the fresh-session path.
            eprintln!(
                "[room] zellij action new-tab failed (exit {:?}) — \
                 cleaning up dead session and starting fresh",
                status.code()
            );
            zellij_delete_session(ZELLIJ_SESSION);
        } else {
            if inside_zellij {
                // We're already inside zellij — assume it's the
                // cc-connect session and the new tab is now visible.
                // If the user happens to be in a different zellij
                // session, they can `zellij attach cc-connect` themselves.
                return Ok(());
            }
            let err = Command::new("zellij")
                .arg("attach")
                .arg(ZELLIJ_SESSION)
                .exec();
            return Err(anyhow!("exec zellij attach failed: {err}"));
        }
    }
    {
        // Fresh session — `-n PATH` forces "create new session with this
        // layout" regardless of any other -session arg semantics.
        let mut cmd = Command::new("zellij");
        cmd.arg("--session").arg(ZELLIJ_SESSION);
        cmd.arg("-n").arg(&layout_path);
        let err = cmd.exec();
        Err(anyhow!("exec zellij failed: {err}"))
    }
}

fn exec_tmux(topic_hex: &str, _claude_args: &[String]) -> Result<()> {
    let claude_wrapper = prepare_claude_wrapper()?;
    let _ = prepare_auto_reply_prompt()?;

    let tmux_script =
        TMUX_LAUNCHER.replace("__CLAUDE_WRAPPER__", &claude_wrapper.to_string_lossy());
    let script_path = write_tmp_layout("cc-connect", "tmux.sh", &tmux_script)?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700));

    print_exit_hint();

    let mut cmd = Command::new("bash");
    cmd.arg(&script_path);
    cmd.env("CC_CONNECT_ROOM", topic_hex);
    let err = cmd.exec();
    Err(anyhow!("exec tmux launcher failed: {err}"))
}

/// State of a named zellij session as reported by `list-sessions`. The
/// `Exited` arm matters because Ctrl-q'd sessions stay in the list as
/// "(EXITED - attach to resurrect)" — treating them as `Live` makes
/// `action new-tab` fail with "There is no active session!".
#[derive(Debug, Clone, Copy)]
enum ZellijSessionState {
    Live,
    Exited,
    Absent,
}

fn zellij_session_state(name: &str) -> ZellijSessionState {
    let out = Command::new("zellij")
        .args(["list-sessions", "--no-formatting"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(o) = out else {
        return ZellijSessionState::Absent;
    };
    let stdout = String::from_utf8_lossy(&o.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.split_whitespace().next() != Some(name) {
            continue;
        }
        if trimmed.contains("EXITED") {
            return ZellijSessionState::Exited;
        }
        return ZellijSessionState::Live;
    }
    ZellijSessionState::Absent
}

/// Best-effort `zellij delete-session NAME`. Used to clear out an
/// EXITED session before creating a fresh one with the same name —
/// otherwise zellij errors with "There is no active session!" when we
/// try to attach.
fn zellij_delete_session(name: &str) {
    let _ = Command::new("zellij")
        .args(["delete-session", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Hint printed to stderr just before exec'ing the multiplexer, so the
/// user knows how to come back to a clean state without hunting for the
/// magic incantation.
fn print_exit_hint() {
    eprintln!("[room] tip:");
    eprintln!("  • Quit zellij with Ctrl-q + y (tmux: Ctrl-b + d to detach).");
    eprintln!("  • `cc-connect clear` stops every chat-daemon + host-bg.");
    eprintln!("  • `cc-connect uninstall` reverses install.sh entirely.");
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
