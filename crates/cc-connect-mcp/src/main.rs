//! cc-connect-mcp — MCP (Model Context Protocol) stdio server.
//!
//! Spawned by Claude Code as a child process when the user has cc-connect
//! configured in `~/.claude/settings.json::mcpServers`. Lets the running
//! Claude itself create / join cc-connect Rooms and participate in their
//! chat substrate without any embedding by `cc-connect-tui` or the
//! VSCode extension's removed Claude pane.
//!
//! Tools fall into three groups:
//!
//! ### Room lifecycle (PR 1, MCP-first model)
//! - `cc_create_room`   — mint a new Room (fork host-bg + chat-daemon, return ticket)
//! - `cc_join_room`     — join by ticket. Files a pending-join awaiting human consent
//! - `cc_leave_room`    — remove a topic from this Claude's bound list
//! - `cc_list_rooms`    — what's this Claude bound to
//! - `cc_set_nick`      — write `~/.cc-connect/config.json::self_nick`
//!
//! ### Chat I/O (existing)
//! - `cc_send`          — broadcast a message
//! - `cc_at`            — broadcast `@<nick> body`
//! - `cc_drop`          — share a local file (iroh-blobs)
//! - `cc_recent`        — last N chat lines
//! - `cc_list_files`    — files dropped into the room
//! - `cc_save_summary`  — overwrite the room's rolling summary
//!
//! ### Trust boundary (PROTOCOL.md §7.3 step 0, ADR-0006)
//!
//! On startup the server walks its parent process chain via
//! `claude_pid::find_claude_ancestor` to find the owning Claude Code
//! PID. Every tool call resolves "which room?" via:
//!   1. an explicit `topic` argument, or
//!   2. the unique entry in `session_state::list_topics(claude_pid)` if
//!      this Claude has joined exactly one Room.
//!
//! Otherwise the call errors. Cross-Claude isolation comes for free:
//! a different Claude Code window has a different PID and reads its own
//! `rooms.json`.
//!
//! `cc_join_room` does NOT directly add a topic to `rooms.json`. It
//! writes a pending-join file the human must `cc-connect accept <token>`
//! (or click Accept in the VSCode panel) to confirm. This closes the
//! prompt-injection pivot SECURITY.md §3 calls out: a hostile chat line
//! convincing Claude to call `cc_join_room("malicious-ticket")` does not
//! by itself subscribe Claude to that hostile Room.
//!
//! Wire format on stdio: newline-delimited JSON-RPC 2.0. One message per
//! line, both for requests and responses. No Content-Length headers (per
//! the MCP stdio transport).

use anyhow::{anyhow, bail, Context, Result};
use cc_connect_core::{claude_pid, session_state};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "cc-connect-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Opportunistic GC: every MCP server startup also prunes orphaned
    // `sessions/by-claude-pid/<pid>/` dirs whose owning Claude exited.
    // Same logic the hook runs every prompt; doing it here too keeps
    // `cc_list_rooms` honest for the current Claude.
    let _ = session_state::prune_dead_pid_sessions();

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.context("read stdin")?;
        if n == 0 {
            return Ok(()); // EOF — Claude Code closed us
        }
        let resp = match handle_message(line.trim()).await {
            Ok(opt) => opt,
            Err(e) => Some(error_response(None, -32603, &format!("internal: {e:#}"))),
        };
        if let Some(resp) = resp {
            let mut out = serde_json::to_vec(&resp)?;
            out.push(b'\n');
            stdout.write_all(&out).await?;
            stdout.flush().await?;
        }
    }
}

async fn handle_message(raw: &str) -> Result<Option<Value>> {
    if raw.is_empty() {
        return Ok(None);
    }
    let req: Value = serde_json::from_str(raw)?;
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|x| x.as_str()).unwrap_or("");

    // Notifications (no id) get no response.
    if method == "notifications/initialized" || method == "notifications/cancelled" {
        return Ok(None);
    }

    match method {
        "initialize" => Ok(Some(success(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION
                }
            }),
        ))),
        "tools/list" => Ok(Some(success(id, json!({ "tools": tool_definitions() })))),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let tool_name = params
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            let outcome = call_tool(&tool_name, arguments).await;
            let result = match outcome {
                Ok(text) => json!({
                    "content": [{ "type": "text", "text": text }]
                }),
                Err(e) => json!({
                    "content": [{ "type": "text", "text": format!("error: {e:#}") }],
                    "isError": true
                }),
            };
            Ok(Some(success(id, result)))
        }
        _ => Ok(Some(error_response(id, -32601, "method not found"))),
    }
}

fn tool_definitions() -> Value {
    json!([
        // ===== Room lifecycle =====
        {
            "name": "cc_create_room",
            "description": "Mint a new cc-connect Room on this machine. Spawns the host-bg gossip bootstrap daemon and a chat-daemon for the new topic, then binds this Claude session to the new Room (so the hook injects chat context into your subsequent prompts). Returns `{topic, ticket}` — share the ticket out-of-band with peers who should join. Use when the user says \"start a new cc-connect room\" or wants to invite collaborators.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "nick": {"type": "string", "description": "Optional: persist this nickname in ~/.cc-connect/config.json::self_nick. Equivalent to calling cc_set_nick first."},
                    "relay": {"type": "string", "description": "Optional: iroh-relay URL (e.g. https://relay.example.com). Defaults to n0's public cluster."}
                }
            }
        },
        {
            "name": "cc_join_room",
            "description": "Request to join an existing cc-connect Room by ticket. Decodes the ticket, ensures a chat-daemon is running for that topic, and writes a pending-join file. **The human must confirm via `cc-connect accept <token>` (CLI watch) or the Accept button (VSCode panel) before this Claude is bound to the Room.** Until then the hook will not inject any chat context for this topic. Returns `{pending_token, topic}`. Use when a peer hands you a `cc1-...` ticket via chat or the user says \"join room <ticket>\".",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticket": {"type": "string", "description": "The `cc1-...` ticket text shared by the room host."},
                    "nick": {"type": "string", "description": "Optional: nickname to display to peers (writes ~/.cc-connect/config.json::self_nick)."}
                },
                "required": ["ticket"]
            }
        },
        {
            "name": "cc_leave_room",
            "description": "Unbind this Claude session from a Room (or from every Room when called with no `topic`). Does NOT stop the chat-daemon — other Claude sessions on this machine may still be using it. Use when the user says \"leave the room\" or \"stop following <topic>\".",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "topic": {"type": "string", "description": "Topic hex to leave. Omit to leave every Room this session is in."}
                }
            }
        },
        {
            "name": "cc_list_rooms",
            "description": "Return the Rooms this Claude session is currently bound to (the topics whose chat the hook injects into your prompts). Each entry includes a 12-char topic prefix and (when available) the ticket the room was created/joined with.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "cc_set_nick",
            "description": "Persist this user's display nickname to ~/.cc-connect/config.json::self_nick. Peers see this Claude's broadcasts as `<nick>-cc`. Without setting this, broadcasts appear as `anonymous-cc`. Idempotent — call once per machine and forget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The nickname (UTF-8, ≤ 64 chars, no whitespace edges)."}
                },
                "required": ["name"]
            }
        },
        // ===== Chat I/O =====
        {
            "name": "cc_send",
            "description": "Broadcast a chat message into a cc-connect Room. Other peers in the room (humans + their AIs) will see it on their next prompt. If this Claude is in only one Room, the topic is implicit; otherwise pass `topic`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "body": {"type": "string", "description": "The message text. UTF-8, max 8 KiB."},
                    "topic": {"type": "string", "description": "Topic hex. Omit when bound to exactly one Room."}
                },
                "required": ["body"]
            }
        },
        {
            "name": "cc_at",
            "description": "Broadcast a message that @-mentions a specific peer by nickname. Equivalent to cc_send with `@<nick> <body>` but more explicit. The recipient's hook tags it `for-you` so their Claude prioritises a reply.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "nick": {"type": "string", "description": "The peer's display name (case-insensitive). Use `cc` to address all Claude instances, `all` / `here` for everyone."},
                    "body": {"type": "string", "description": "The message text."},
                    "topic": {"type": "string", "description": "Topic hex. Omit when bound to exactly one Room."}
                },
                "required": ["nick", "body"]
            }
        },
        {
            "name": "cc_drop",
            "description": "Share a local file with all peers in the bound Room. The file is hashed via iroh-blobs and announced; peers fetch it on demand. Their Claude sees it as an `@file:` reference on the next prompt. Common credential paths (~/.ssh, ~/.aws, .env*, *.pem, …) are refused.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute or relative path on this machine."},
                    "topic": {"type": "string", "description": "Topic hex. Omit when bound to exactly one Room."}
                },
                "required": ["path"]
            }
        },
        {
            "name": "cc_recent",
            "description": "Return the most recent chat lines from the bound Room's log (most recent last). Useful when Claude wants more context than what the hook injected.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "How many trailing lines (default 20, max 200).", "minimum": 1, "maximum": 200},
                    "topic": {"type": "string", "description": "Topic hex. Omit when bound to exactly one Room."}
                }
            }
        },
        {
            "name": "cc_list_files",
            "description": "List files dropped into the bound Room (most recent first). Each entry includes the local path so Claude can Read it directly.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "Cap on the number of entries (default 50, max 500).", "minimum": 1, "maximum": 500},
                    "topic": {"type": "string", "description": "Topic hex. Omit when bound to exactly one Room."}
                }
            }
        },
        {
            "name": "cc_save_summary",
            "description": "Overwrite the bound Room's rolling summary at ~/.cc-connect/rooms/<topic>/summary.md. The hook injects this summary into every prompt's context so future Claude instances pick up long-running room state without burning their token budget on raw history. Use after digesting a chunk of conversation; keep summaries terse (≤ 1 KiB).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "Markdown summary text. Capped at 64 KiB on the server side."},
                    "topic": {"type": "string", "description": "Topic hex. Omit when bound to exactly one Room."}
                },
                "required": ["text"]
            }
        }
    ])
}

async fn call_tool(name: &str, args: Value) -> Result<String> {
    match name {
        // ===== Room lifecycle =====
        "cc_create_room" => tool_create_room(args).await,
        "cc_join_room" => tool_join_room(args).await,
        "cc_leave_room" => tool_leave_room(args).await,
        "cc_list_rooms" => tool_list_rooms().await,
        "cc_set_nick" => tool_set_nick(args).await,

        // ===== Chat I/O =====
        "cc_send" => {
            let body = required_str(&args, "body")?;
            let topic = resolve_topic(&args)?;
            ipc_call(&topic, json!({"action": "send", "body": body})).await?;
            Ok(format!(
                "sent ({} bytes) to topic {}",
                body.len(),
                short(&topic)
            ))
        }
        "cc_at" => {
            let nick = required_str(&args, "nick")?;
            let body = required_str(&args, "body")?;
            let topic = resolve_topic(&args)?;
            ipc_call(&topic, json!({"action": "at", "nick": nick, "body": body})).await?;
            Ok(format!("sent @{nick} to topic {}: {body}", short(&topic)))
        }
        "cc_drop" => {
            let path = required_str(&args, "path")?;
            let topic = resolve_topic(&args)?;
            ipc_call(&topic, json!({"action": "drop", "path": path})).await?;
            Ok(format!("dropped {path} into topic {}", short(&topic)))
        }
        "cc_recent" => {
            let limit = args.get("limit").and_then(|x| x.as_u64()).unwrap_or(20);
            let topic = resolve_topic(&args)?;
            let resp = ipc_call(&topic, json!({"action": "recent", "limit": limit})).await?;
            let messages = resp.get("messages").cloned().unwrap_or_else(|| json!([]));
            Ok(format!(
                "recent ({}) in topic {}:\n{}",
                messages.as_array().map(|a| a.len()).unwrap_or(0),
                short(&topic),
                serde_json::to_string_pretty(&messages).unwrap_or_default()
            ))
        }
        "cc_list_files" => {
            let limit = args.get("limit").and_then(|x| x.as_u64()).unwrap_or(50);
            let topic = resolve_topic(&args)?;
            let resp = ipc_call(&topic, json!({"action": "list_files", "limit": limit})).await?;
            let files = resp.get("files").cloned().unwrap_or_else(|| json!([]));
            Ok(format!(
                "files ({}) in topic {}:\n{}",
                files.as_array().map(|a| a.len()).unwrap_or(0),
                short(&topic),
                serde_json::to_string_pretty(&files).unwrap_or_default()
            ))
        }
        "cc_save_summary" => {
            let text = required_str(&args, "text")?;
            let topic = resolve_topic(&args)?;
            ipc_call(&topic, json!({"action": "save_summary", "text": text})).await?;
            Ok(format!(
                "summary saved ({} bytes) for topic {}",
                text.len(),
                short(&topic)
            ))
        }
        other => bail!("unknown tool: {other}"),
    }
}

// ============================================================================
// Room-lifecycle tool implementations
// ============================================================================

async fn tool_create_room(args: Value) -> Result<String> {
    if let Some(name) = args.get("nick").and_then(|x| x.as_str()) {
        if !name.is_empty() {
            write_self_nick(name)?;
        }
    }
    let relay = args.get("relay").and_then(|x| x.as_str());

    // 1. Spawn host-bg → get ticket.
    let (topic, ticket) = spawn_host_bg(relay).await?;

    // 2. Spawn chat-daemon for the same ticket (idempotent ALREADY path).
    spawn_chat_daemon(&ticket, /*no_relay=*/ false, relay).await?;

    // 3. Bind this Claude session to the new topic.
    let claude_pid = our_claude_pid()?;
    session_state::add_topic(claude_pid, &topic)?;

    Ok(serde_json::to_string_pretty(&json!({
        "topic": topic,
        "topic_short": short(&topic),
        "ticket": ticket,
    }))
    .unwrap())
}

async fn tool_join_room(args: Value) -> Result<String> {
    let ticket = required_str(&args, "ticket")?.trim().to_string();

    if let Some(name) = args.get("nick").and_then(|x| x.as_str()) {
        if !name.is_empty() {
            write_self_nick(name)?;
        }
    }

    // Spawn the chat-daemon (idempotent — `ALREADY <topic> <pid>` if
    // already running) and parse the topic_hex from its READY/ALREADY
    // line. We use this rather than decoding the ticket ourselves
    // because that requires iroh deps the MCP crate doesn't have.
    let topic = spawn_chat_daemon(&ticket, /*no_relay=*/ false, None).await?;

    // File the consent gate. NOT added to rooms.json — the human must
    // run `cc-connect accept <token>` (CLI watch) or click Accept in
    // the VSCode panel.
    let claude_pid = our_claude_pid()?;
    let token = session_state::create_pending_join(claude_pid, &topic, &ticket)?;

    Ok(serde_json::to_string_pretty(&json!({
        "pending_token": token,
        "topic": topic,
        "topic_short": short(&topic),
        "next_step": format!(
            "Human must run `cc-connect accept {token}` (or click Accept in the cc-connect VSCode panel) to bind this Claude to the room."
        ),
    }))
    .unwrap())
}

async fn tool_leave_room(args: Value) -> Result<String> {
    let claude_pid = our_claude_pid()?;
    if let Some(topic) = args.get("topic").and_then(|x| x.as_str()) {
        session_state::remove_topic(claude_pid, topic)?;
        Ok(format!("left topic {}", short(topic)))
    } else {
        let before = session_state::list_topics(claude_pid).unwrap_or_default();
        session_state::remove_all_topics(claude_pid)?;
        Ok(format!("left all rooms ({} topic(s))", before.len()))
    }
}

async fn tool_list_rooms() -> Result<String> {
    let claude_pid = our_claude_pid()?;
    let topics = session_state::list_topics(claude_pid)?;
    let entries: Vec<Value> = topics
        .into_iter()
        .map(|t| {
            let socket_marker = home_dir()
                .map(|h| {
                    h.join(".cc-connect")
                        .join("rooms")
                        .join(&t)
                        .join("chat.sock")
                })
                .ok();
            let socket_alive = socket_marker.as_ref().map(|p| p.exists()).unwrap_or(false);
            json!({
                "topic": t,
                "topic_short": short(&t),
                "chat_daemon_alive": socket_alive,
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&json!({
        "claude_pid": claude_pid,
        "rooms": entries,
    }))
    .unwrap())
}

async fn tool_set_nick(args: Value) -> Result<String> {
    let raw = required_str(&args, "name")?.trim();
    if raw.is_empty() {
        bail!("nick must not be empty");
    }
    if raw.len() > 64 {
        bail!("nick must be ≤ 64 bytes (got {})", raw.len());
    }
    write_self_nick(raw)?;
    Ok(format!(
        "self_nick set to `{raw}` (peers will see your AI as `{raw}-cc`)"
    ))
}

// ============================================================================
// Topic resolution + Claude PID lookup
// ============================================================================

/// Cached on first call: walking the parent chain is a few syscalls but
/// the result never changes for the lifetime of the MCP server (PPID is
/// stable until the parent dies, in which case we'd be reparented to
/// init and dead anyway).
fn our_claude_pid() -> Result<u32> {
    static CACHE: OnceLock<u32> = OnceLock::new();
    if let Some(p) = CACHE.get() {
        return Ok(*p);
    }
    let pid = claude_pid::find_claude_ancestor(std::process::id())
        .context("locate owning claude binary in process tree")?;
    let _ = CACHE.set(pid);
    Ok(pid)
}

/// Resolve which Room a chat-I/O call should target.
///
///   1. Explicit `topic` arg wins.
///   2. Otherwise look up `session_state::list_topics(our_claude_pid)`.
///      If exactly one Room is bound, use it.
///   3. Otherwise error with a helpful message naming the candidates.
fn resolve_topic(args: &Value) -> Result<String> {
    if let Some(t) = args.get("topic").and_then(|x| x.as_str()) {
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    let claude_pid = our_claude_pid()?;
    let topics = session_state::list_topics(claude_pid)?;
    match topics.len() {
        0 => bail!(
            "this Claude is not bound to any cc-connect Room. \
             Call cc_create_room(...) or cc_join_room(ticket=...) first."
        ),
        1 => Ok(topics.into_iter().next().unwrap()),
        n => {
            let names = topics
                .iter()
                .map(|t| short(t))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("this Claude is bound to {n} Rooms ({names}); pass `topic` to disambiguate")
        }
    }
}

// ============================================================================
// Subprocess helpers — spawn cc-connect's host-bg + chat-daemon entry points
// ============================================================================

/// Locate the `cc-connect` binary. Prefers a sibling next to the running
/// `cc-connect-mcp` (the install layout `~/.local/bin/`); falls back to
/// PATH lookup so unusual install layouts still work.
fn cc_connect_bin() -> PathBuf {
    if let Ok(self_path) = std::env::current_exe() {
        if let Some(parent) = self_path.parent() {
            let sibling = parent.join("cc-connect");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("cc-connect")
}

/// Spawn `cc-connect host-bg start [--relay url]`, wait for the
/// `READY <topic_hex> <ticket>` line, return both. The host-bg-daemon
/// continues running detached — it's reaped by init when this MCP
/// server (or its parent claude) eventually exits, or by an explicit
/// `cc-connect host-bg stop <topic>` call.
async fn spawn_host_bg(relay: Option<&str>) -> Result<(String, String)> {
    let mut cmd = tokio::process::Command::new(cc_connect_bin());
    cmd.arg("host-bg").arg("start");
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("spawn cc-connect host-bg start")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("cc-connect stdout pipe missing"))?;
    let stderr = child.stderr.take();

    // The first line is the daemon's READY response. Subsequent stdout
    // lines (the friendly "Daemon hosting room ..." block) are noise we
    // don't need.
    let mut reader = BufReader::new(stdout).lines();
    let first =
        match tokio::time::timeout(std::time::Duration::from_secs(15), reader.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => bail!("cc-connect host-bg start exited without printing READY"),
            Ok(Err(e)) => bail!("read host-bg stdout: {e}"),
            Err(_) => {
                let _ = child.start_kill();
                let stderr_text = drain_stderr(stderr).await;
                bail!(
                "cc-connect host-bg start did not print READY within 15s. stderr:\n{stderr_text}"
            );
            }
        };

    // Don't await child — host-bg start has already detached the daemon.
    // The wrapping `cc-connect host-bg start` process itself exits after
    // printing its informational block; we let it complete in the
    // background.
    drop(child);

    let trimmed = first.trim();
    let rest = trimmed
        .strip_prefix("READY ")
        .ok_or_else(|| anyhow!("expected READY line, got: {trimmed:?}"))?;
    let mut parts = rest.splitn(2, ' ');
    let topic = parts
        .next()
        .ok_or_else(|| anyhow!("READY line missing topic: {trimmed:?}"))?
        .to_string();
    let ticket = parts
        .next()
        .ok_or_else(|| anyhow!("READY line missing ticket: {trimmed:?}"))?
        .to_string();
    Ok((topic, ticket))
}

/// Spawn `cc-connect chat-daemon start <ticket>`. Idempotent: if a
/// chat-daemon for this topic is already running, the wrapper prints
/// `ALREADY <topic> <pid>` and exits 0; we treat that as success.
///
/// Returns the topic_hex parsed from the daemon's first stdout line:
///   - fresh start: `READY <topic_hex>`
///   - already running: `ALREADY <topic_hex> <pid>`
async fn spawn_chat_daemon(ticket: &str, no_relay: bool, relay: Option<&str>) -> Result<String> {
    let mut cmd = tokio::process::Command::new(cc_connect_bin());
    cmd.arg("chat-daemon").arg("start").arg(ticket);
    if no_relay {
        cmd.arg("--no-relay");
    }
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("spawn cc-connect chat-daemon start")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("cc-connect stdout pipe missing"))?;
    let stderr = child.stderr.take();

    let mut reader = BufReader::new(stdout).lines();
    let first =
        match tokio::time::timeout(std::time::Duration::from_secs(15), reader.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => bail!("cc-connect chat-daemon start exited without output"),
            Ok(Err(e)) => bail!("read chat-daemon stdout: {e}"),
            Err(_) => {
                let _ = child.start_kill();
                let stderr_text = drain_stderr(stderr).await;
                bail!("cc-connect chat-daemon start timed out. stderr:\n{stderr_text}");
            }
        };
    drop(child);

    let trimmed = first.trim();
    if let Some(rest) = trimmed.strip_prefix("READY ") {
        // `READY <topic_hex>` — fresh start. The topic is the whole rest.
        Ok(rest.trim().to_string())
    } else if let Some(rest) = trimmed.strip_prefix("ALREADY ") {
        // `ALREADY <topic_hex> <pid>` — idempotent re-start. Take the
        // first whitespace-separated field.
        let topic = rest
            .split_whitespace()
            .next()
            .ok_or_else(|| anyhow!("ALREADY line missing topic: {trimmed:?}"))?;
        Ok(topic.to_string())
    } else {
        bail!("expected READY|ALREADY from chat-daemon, got: {trimmed:?}")
    }
}

async fn drain_stderr(stderr: Option<tokio::process::ChildStderr>) -> String {
    use tokio::io::AsyncReadExt;
    let Some(mut s) = stderr else {
        return String::new();
    };
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf).await;
    buf
}

// ============================================================================
// IPC to chat-daemon (per-topic socket)
// ============================================================================

/// Send one command to the named topic's chat-session IPC socket and
/// return the `data` field from its response.
async fn ipc_call(topic: &str, payload: Value) -> Result<Value> {
    let socket = ipc_socket_path(topic)?;
    if !socket.exists() {
        bail!(
            "chat-daemon socket missing for topic {} (marker {} resolved to a non-existent path — daemon crashed?)",
            short(topic),
            socket.display()
        );
    }
    let stream = UnixStream::connect(&socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    let (read_half, mut write_half) = stream.into_split();
    let mut req = serde_json::to_vec(&payload)?;
    req.push(b'\n');
    write_half.write_all(&req).await?;
    write_half.flush().await?;
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let resp: Value = serde_json::from_str(line.trim()).context("parse IPC response")?;
    if !resp.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
        let err = resp
            .get("err")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        bail!("IPC error: {err}");
    }
    Ok(resp.get("data").cloned().unwrap_or(json!({})))
}

fn ipc_socket_path(topic: &str) -> Result<PathBuf> {
    if topic.is_empty() {
        bail!("topic is empty");
    }
    let home = home_dir()?;
    let marker = home
        .join(".cc-connect")
        .join("rooms")
        .join(topic)
        .join("chat.sock");
    if !marker.exists() {
        bail!(
            "no active chat-daemon for topic {} (marker {} missing)",
            short(topic),
            marker.display()
        );
    }
    let raw =
        std::fs::read_to_string(&marker).with_context(|| format!("read {}", marker.display()))?;
    Ok(PathBuf::from(raw.trim()))
}

// ============================================================================
// Misc helpers
// ============================================================================

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME env var not set"))
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("missing or non-string `{key}`"))
}

/// 12-char prefix of a topic hex for log lines and error messages.
fn short(topic: &str) -> String {
    topic.chars().take(12).collect()
}

fn write_self_nick(name: &str) -> Result<()> {
    let path = home_dir()?.join(".cc-connect").join("config.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert("self_nick".to_string(), json!(name));
    }
    let pretty = serde_json::to_vec_pretty(&value)?;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    use std::io::Write as _;
    file.write_all(&pretty)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

// ---- JSON-RPC envelope helpers ---------------------------------------------

fn success(id: Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
}
