// Read the chat-daemon PID file to surface the room ticket so the chat-ui
// can show it (and let the user Ctrl-Y copy it to the clipboard).

import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ChatDaemonPidFile } from "./types.ts";

export function readChatDaemonPidFile(topic: string): ChatDaemonPidFile | null {
  const path = join(homedir(), ".cc-connect", "rooms", topic, "chat-daemon.pid");
  try {
    const raw = readFileSync(path, "utf8");
    return JSON.parse(raw.trim()) as ChatDaemonPidFile;
  } catch {
    return null;
  }
}

export function readSelfNick(): string | null {
  try {
    const raw = readFileSync(join(homedir(), ".cc-connect", "config.json"), "utf8");
    const cfg = JSON.parse(raw) as { self_nick?: string };
    return cfg.self_nick && cfg.self_nick.length > 0 ? cfg.self_nick : null;
  } catch {
    return null;
  }
}

export function readSelfPubkey(): string | null {
  // Identity is 32 raw seed bytes. We only need to read the file to
  // confirm presence + match `msg.author` exactly; pubkey derivation is
  // server-side. For UI purposes ("did I send this?") we use nicknames.
  return null;
}
