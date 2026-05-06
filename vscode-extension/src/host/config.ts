// Read/write `~/.cc-connect/config.json` from the extension host.
//
// The Rust side (`crates/cc-connect/src/setup.rs`) is the canonical
// owner of this file; we mirror just the parts the extension cares
// about (`self_nick`) and preserve every unknown field on write so
// `relay_mode` / `owner_only_mentions` / future Rust-side keys
// survive round-trips.

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

const CONFIG_DIR = path.join(os.homedir(), '.cc-connect');
const CONFIG_PATH = path.join(CONFIG_DIR, 'config.json');

// Mirror of `cc_connect_core::message::NICK_MAX_BYTES`.
export const NICK_MAX_BYTES = 64;

type ConnectConfig = {
  self_nick?: string;
  [key: string]: unknown;
};

function readConfig(): ConnectConfig {
  try {
    const raw = fs.readFileSync(CONFIG_PATH, 'utf8');
    if (!raw.trim()) return {};
    return JSON.parse(raw) as ConnectConfig;
  } catch {
    return {};
  }
}

function writeConfig(cfg: ConnectConfig): void {
  fs.mkdirSync(CONFIG_DIR, { recursive: true, mode: 0o700 });
  fs.writeFileSync(CONFIG_PATH, JSON.stringify(cfg, null, 2) + '\n', {
    mode: 0o600,
  });
}

/** Returns the persisted nick, or undefined if absent. An empty string
 *  on disk means the user explicitly opted out — we treat that as
 *  "configured, no nick" and return undefined too (so peers see the
 *  pubkey prefix). Use `selfNickConfigured()` to distinguish the
 *  never-asked-yet case. */
export function readSelfNick(): string | undefined {
  const cfg = readConfig();
  if (typeof cfg.self_nick !== 'string') return undefined;
  return cfg.self_nick.length > 0 ? cfg.self_nick : undefined;
}

/** True iff `self_nick` exists in config.json (even as an empty string).
 *  Used to decide whether to prompt the user on first room start. */
export function selfNickConfigured(): boolean {
  const cfg = readConfig();
  return typeof cfg.self_nick === 'string';
}

export function validateNick(raw: string): string | undefined {
  const trimmed = raw.trim();
  if (Buffer.byteLength(trimmed, 'utf8') > NICK_MAX_BYTES) {
    return `Nickname too long (max ${NICK_MAX_BYTES} bytes).`;
  }
  
  if (/[\u0000-\u001f\u007f]/.test(trimmed)) {
    return 'Nickname must not contain control characters.';
  }
  return undefined;
}

/** Persist `self_nick`. Empty string is allowed and means "no nick".
 *  Other keys in config.json are preserved untouched. */
export function writeSelfNick(nick: string): void {
  const trimmed = nick.trim();
  const err = validateNick(trimmed);
  if (err) throw new Error(err);
  const cfg = readConfig();
  cfg.self_nick = trimmed;
  writeConfig(cfg);
}
