// Wrappers around the `cc-connect host-bg start` and
// `cc-connect chat-daemon start <ticket>` CLI commands. Both exit
// quickly after printing a single line on stdout; we capture and
// parse, then return.
//
// Per design §4.4, the binary path is resolved by absolute path under
// ~/.local/bin/ so this works under macOS GUI launches where the
// extension host doesn't inherit the user's shell PATH.

import { spawn } from 'node:child_process';
import { homedir } from 'node:os';
import { join } from 'node:path';

const CC_BIN = join(homedir(), '.local', 'bin', 'cc-connect');

interface RunResult {
  stdout: string;
  stderr: string;
  code: number;
}

function run(args: string[]): Promise<RunResult> {
  return new Promise((resolve, reject) => {
    const p = spawn(CC_BIN, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    p.stdout.on('data', (c: Buffer) => (stdout += c.toString('utf8')));
    p.stderr.on('data', (c: Buffer) => (stderr += c.toString('utf8')));
    p.on('error', (e) =>
      reject(
        new Error(
          `spawn ${CC_BIN}: ${e.message} (is cc-connect installed at ~/.local/bin/?)`,
        ),
      ),
    );
    p.on('exit', (code) => resolve({ stdout, stderr, code: code ?? -1 }));
  });
}

/** Spawn `cc-connect host-bg start` and return the printed Ticket.
 *  The daemon stays running detached; this call exits as soon as the
 *  Ticket is on stdout. */
export async function startHostBg(): Promise<string> {
  const r = await run(['host-bg', 'start']);
  if (r.code !== 0) {
    throw new Error(
      `host-bg start exited ${r.code}: ${(r.stderr || r.stdout).trim()}`,
    );
  }
  const ticket = r.stdout.trim();
  if (!ticket.startsWith('cc1-')) {
    throw new Error(
      `unexpected host-bg stdout: ${ticket.slice(0, 80)}`,
    );
  }
  return ticket;
}

/** Spawn `cc-connect chat-daemon start <ticket>` and return the bound
 *  topic hex. Idempotent: if a daemon already owns the topic, the CLI
 *  prints `ALREADY <topic> <pid>` and exits 0; otherwise prints
 *  `READY <topic>`. Either way we extract the topic. */
export async function startChatDaemon(ticket: string): Promise<string> {
  const r = await run(['chat-daemon', 'start', ticket]);
  if (r.code !== 0) {
    throw new Error(
      `chat-daemon start exited ${r.code}: ${(r.stderr || r.stdout).trim()}`,
    );
  }
  const line = r.stdout
    .split('\n')
    .map((l) => l.trim())
    .find((l) => l.startsWith('READY ') || l.startsWith('ALREADY '));
  if (!line) {
    throw new Error(
      `chat-daemon start: no READY/ALREADY line in stdout: ${r.stdout.slice(0, 200)}`,
    );
  }
  const parts = line.split(/\s+/);
  // READY <topic>     |     ALREADY <topic> <pid>
  if (parts.length < 2 || !parts[1]) {
    throw new Error(`chat-daemon start: malformed status line: ${line}`);
  }
  return parts[1];
}
