// chat.sock client. The chat-daemon's IPC server lives at the path stored
// in the marker file `~/.cc-connect/rooms/<topic>/chat.sock` (the actual
// socket is under `/tmp/cc-<uid>-<rand>.sock` to stay within macOS's
// SUN_LEN). Wire format: one JSON request per line, one JSON response
// per line.

import { connect, type Socket } from "node:net";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

interface IpcResponse {
  ok: boolean;
  err?: string;
  data?: Record<string, unknown>;
}

/** Resolve the absolute socket path for a topic via the marker file
 *  written by chat_session at `~/.cc-connect/rooms/<topic>/chat.sock`. */
export function resolveSocketPath(topic: string): string | null {
  const marker = join(homedir(), ".cc-connect", "rooms", topic, "chat.sock");
  try {
    return readFileSync(marker, "utf8").trim();
  } catch {
    return null;
  }
}

/** Send one command, await one response. Each call opens a fresh
 *  connection — same pattern cc-connect-mcp uses, keeps the protocol
 *  simple and avoids long-lived state on the client side. */
export async function ipcCall(
  topic: string,
  payload: Record<string, unknown>,
): Promise<IpcResponse> {
  const sock = resolveSocketPath(topic);
  if (sock === null) {
    return { ok: false, err: `no chat.sock marker for topic ${topic.slice(0, 12)}…` };
  }
  return new Promise((resolve) => {
    const conn: Socket = connect(sock);
    let buf = "";
    let resolved = false;
    const finish = (resp: IpcResponse) => {
      if (resolved) return;
      resolved = true;
      try {
        conn.end();
      } catch {}
      resolve(resp);
    };

    conn.on("connect", () => {
      conn.write(JSON.stringify(payload) + "\n");
    });
    conn.on("data", (chunk: Buffer) => {
      buf += chunk.toString("utf8");
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      const line = buf.slice(0, nl);
      try {
        const parsed = JSON.parse(line) as IpcResponse;
        finish(parsed);
      } catch (e) {
        finish({ ok: false, err: `parse: ${(e as Error).message}` });
      }
    });
    conn.on("error", (e) => {
      finish({ ok: false, err: e.message });
    });
    conn.on("close", () => {
      finish({ ok: false, err: "socket closed before response" });
    });
  });
}

// ---- typed helpers -------------------------------------------------------

// `source: "human"` tells chat_session this came from a human typing in
// chat-ui (not from an AI's MCP-driven send). The dispatch routes onto
// the InputSource::Local channel so peers see the bare nick (no `-cc`
// suffix) and owner @-mention wakeups fire correctly. cc-connect-mcp
// omits this field so it defaults to InputSource::Mcp / `<nick>-cc`.

export async function ccSend(topic: string, body: string): Promise<IpcResponse> {
  return ipcCall(topic, { action: "send", body, source: "human" });
}

export async function ccAt(
  topic: string,
  nick: string,
  body: string,
): Promise<IpcResponse> {
  return ipcCall(topic, { action: "at", nick, body, source: "human" });
}

export async function ccDrop(topic: string, path: string): Promise<IpcResponse> {
  return ipcCall(topic, { action: "drop", path, source: "human" });
}

export async function ccRecent(topic: string, limit = 20): Promise<IpcResponse> {
  return ipcCall(topic, { action: "recent", limit });
}
