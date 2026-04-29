//! cc-connect-mcp — MCP (Model Context Protocol) stdio server.
//!
//! Spawned by Claude Code as a child process when the user has cc-connect
//! configured in `~/.claude/settings.json::mcpServers`. Exposes five tools
//! that Claude can call mid-conversation:
//!
//! - `cc_send`        — broadcast a chat message into the bound room
//! - `cc_at`          — broadcast `@<nick> <body>` (mention helper)
//! - `cc_drop`        — share a local file with all peers
//! - `cc_recent`      — last N chat lines (raw log)
//! - `cc_list_files`  — files dropped into the bound room
//!
//! Routing: the server reads `CC_CONNECT_ROOM` (set by `cc-connect-tui`),
//! dials `/tmp/cc-connect-$UID/sockets/<topic>.sock`, forwards the
//! command, returns the response. If `CC_CONNECT_ROOM` is unset the
//! tools error out cleanly with "no active cc-connect room".
//!
//! Wire format on stdio: newline-delimited JSON-RPC 2.0. One message per
//! line, both for requests and responses. No Content-Length headers (per
//! the MCP stdio transport).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "cc-connect-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
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
        "tools/list" => Ok(Some(success(
            id,
            json!({ "tools": tool_definitions() }),
        ))),
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
                    "content": [{ "type": "text", "text": format!("error: {e}") }],
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
        {
            "name": "cc_send",
            "description": "Broadcast a chat message into the cc-connect room this Claude Code instance is bound to. Other peers in the room (humans + their AIs) will see it on their next prompt. Use to update the team, ask a question, or volunteer information.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "body": {"type": "string", "description": "The message text. UTF-8, max 8 KiB."}
                },
                "required": ["body"]
            }
        },
        {
            "name": "cc_at",
            "description": "Broadcast a message that @-mentions a specific peer by nickname. Equivalent to cc_send with `@<nick> <body>` but more explicit. The recipient's TUI highlights mention lines and their hook tags them `for-you` so their Claude prioritises.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "nick": {"type": "string", "description": "The peer's display name (case-insensitive). Use `cc` to address all Claude instances, `all` / `here` for everyone."},
                    "body": {"type": "string", "description": "The message text."}
                },
                "required": ["nick", "body"]
            }
        },
        {
            "name": "cc_drop",
            "description": "Share a local file with all peers in the room. The file is hashed via iroh-blobs and announced; peers fetch it on demand. Their Claude sees it as an `@file:` reference on the next prompt.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute or relative path on this machine."}
                },
                "required": ["path"]
            }
        },
        {
            "name": "cc_recent",
            "description": "Return the most recent chat lines from the room's log (most recent last). Useful when Claude wants more context than what the hook injected.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "How many trailing lines (default 20, max 200).", "minimum": 1, "maximum": 200}
                }
            }
        },
        {
            "name": "cc_list_files",
            "description": "List files dropped into the room (most recent first). Each entry includes the local path so Claude can Read it directly.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "Cap on the number of entries (default 50, max 500).", "minimum": 1, "maximum": 500}
                }
            }
        },
        {
            "name": "cc_save_summary",
            "description": "Overwrite the room's rolling summary at ~/.cc-connect/rooms/<topic>/summary.md. The hook injects this summary into every prompt's context so future Claude instances pick up long-running room state without burning their token budget on raw history. Use after digesting a chunk of conversation; keep summaries terse (≤ 1 KiB).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "Markdown summary text. Capped at 64 KiB on the server side."}
                },
                "required": ["text"]
            }
        }
    ])
}

async fn call_tool(name: &str, args: Value) -> Result<String> {
    match name {
        "cc_send" => {
            let body = args
                .get("body")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow!("missing `body`"))?;
            ipc_call(json!({"action": "send", "body": body})).await?;
            Ok(format!("sent ({} bytes)", body.len()))
        }
        "cc_at" => {
            let nick = args
                .get("nick")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow!("missing `nick`"))?;
            let body = args
                .get("body")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow!("missing `body`"))?;
            ipc_call(json!({"action": "at", "nick": nick, "body": body})).await?;
            Ok(format!("sent @{nick}: {body}"))
        }
        "cc_drop" => {
            let path = args
                .get("path")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow!("missing `path`"))?;
            ipc_call(json!({"action": "drop", "path": path})).await?;
            Ok(format!("dropped {path}"))
        }
        "cc_recent" => {
            let limit = args.get("limit").and_then(|x| x.as_u64()).unwrap_or(20);
            let resp = ipc_call(json!({"action": "recent", "limit": limit})).await?;
            let messages = resp
                .get("messages")
                .cloned()
                .unwrap_or_else(|| json!([]));
            Ok(format!(
                "recent ({}):\n{}",
                messages.as_array().map(|a| a.len()).unwrap_or(0),
                serde_json::to_string_pretty(&messages).unwrap_or_default()
            ))
        }
        "cc_list_files" => {
            let limit = args.get("limit").and_then(|x| x.as_u64()).unwrap_or(50);
            let resp = ipc_call(json!({"action": "list_files", "limit": limit})).await?;
            let files = resp.get("files").cloned().unwrap_or_else(|| json!([]));
            Ok(format!(
                "files ({}):\n{}",
                files.as_array().map(|a| a.len()).unwrap_or(0),
                serde_json::to_string_pretty(&files).unwrap_or_default()
            ))
        }
        "cc_save_summary" => {
            let text = args
                .get("text")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow!("missing `text`"))?;
            ipc_call(json!({"action": "save_summary", "text": text})).await?;
            Ok(format!("summary saved ({} bytes)", text.len()))
        }
        other => bail!("unknown tool: {other}"),
    }
}

/// Send one command to the chat session's IPC socket and return the
/// `data` field from its response (an empty object on action-only
/// commands).
async fn ipc_call(payload: Value) -> Result<Value> {
    let socket = ipc_socket_path()?;
    if !socket.exists() {
        bail!(
            "no active cc-connect room (socket {} missing — is `cc-connect chat` / `cc-connect-tui` running and bound to topic {}?)",
            socket.display(),
            std::env::var("CC_CONNECT_ROOM").unwrap_or_else(|_| "<unset>".to_string())
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

fn ipc_socket_path() -> Result<PathBuf> {
    let topic = std::env::var("CC_CONNECT_ROOM").map_err(|_| {
        anyhow!("CC_CONNECT_ROOM env var not set — Claude Code wasn't launched by `cc-connect-tui`")
    })?;
    if topic.is_empty() {
        bail!("CC_CONNECT_ROOM is empty");
    }
    // chat_session writes the absolute socket path to a HOME-side marker
    // (because the actual socket lives under /tmp to stay within the macOS
    // SUN_LEN limit, but the room's HOME is where each peer's state lives).
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set"))?;
    let marker = home
        .join(".cc-connect")
        .join("rooms")
        .join(&topic)
        .join("chat.sock");
    if !marker.exists() {
        bail!(
            "no active cc-connect room (marker {} missing — is `cc-connect chat` / `cc-connect-tui` running for topic {topic}?)",
            marker.display()
        );
    }
    let raw = std::fs::read_to_string(&marker)
        .with_context(|| format!("read {}", marker.display()))?;
    Ok(PathBuf::from(raw.trim()))
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
