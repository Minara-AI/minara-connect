// Per-Room Claude Agent SDK driver. Each Room view owns one runner.
// `enqueue()` accepts a prompt (typically the body of an @-mention);
// the runner serialises turns through a single in-flight `query()`
// at a time, threading `--session-id` so the cc-connect-hook's
// per-(Room, Session) Cursor advances correctly across calls.
//
// SDK call shape:
//   - first turn: `sessionId: <uuid>` to mint the Session with our UUID
//   - subsequent turns: `resume: <uuid>` to pick up the same Session
//   - `env.CC_CONNECT_ROOM = <topic>` so the hook gates injection
//   - `pathToClaudeCodeExecutable` resolves macOS-GUI launch PATH
//   - `includeHookEvents: true` exposes hook lifecycle events in the
//     stream so the Claude panel can render them
//   - per-turn `AbortController` so `interrupt()` can kill the
//     in-flight turn without tearing down the whole runner
//
// Permission UI and MCP cc-connect server registration land in
// subsequent steps.

import { randomUUID } from 'node:crypto';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { query } from '@anthropic-ai/claude-agent-sdk';

export interface ClaudeRunnerState {
  busy: boolean;
  queued: number;
}

export interface ClaudeRunnerOptions {
  topic: string;
  onEvent: (event: unknown) => void;
  onStateChange: (state: ClaudeRunnerState) => void;
}

export interface ClaudeRunnerHandle {
  /** Enqueue a prompt. Runs after any currently-in-flight turn. */
  enqueue(prompt: string): void;
  /** Cancel the currently-running turn. Queued items still run. */
  interrupt(): void;
  /** Mint a fresh Session: drop queue, abort in-flight, rotate the
   *  sessionId so the next turn starts clean. */
  resetSession(): void;
  /** Tear the runner down: cancel current + clear queue. Used on
   *  panel dispose. */
  abort(): void;
}

export function createClaudeRunner(
  opts: ClaudeRunnerOptions,
): ClaudeRunnerHandle {
  let sessionUuid = randomUUID();
  const claudeBin = join(homedir(), '.local', 'bin', 'claude');
  let hasStarted = false;
  const queue: string[] = [];
  let processing = false;
  let panelClosed = false;
  let currentTurnAc: AbortController | null = null;

  function publishState(): void {
    opts.onStateChange({ busy: processing, queued: queue.length });
  }

  async function runOne(prompt: string): Promise<void> {
    const ac = new AbortController();
    currentTurnAc = ac;
    const sessionOpt = hasStarted
      ? { resume: sessionUuid }
      : { sessionId: sessionUuid };
    const q = query({
      prompt,
      options: {
        ...sessionOpt,
        pathToClaudeCodeExecutable: claudeBin,
        includeHookEvents: true,
        abortController: ac,
        env: { ...process.env, CC_CONNECT_ROOM: opts.topic },
        // v0 trust posture: bypass all permission dialogs. Real
        // per-tool approval UI tracked as design §8 / claude-ui-polish T1.3.
        permissionMode: 'bypassPermissions',
      },
    });
    hasStarted = true;
    try {
      for await (const evt of q) {
        if (ac.signal.aborted) break;
        opts.onEvent(evt);
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (!/abort/i.test(msg)) {
        opts.onEvent({ type: 'sdk:error', error: msg });
      }
    } finally {
      if (currentTurnAc === ac) currentTurnAc = null;
    }
  }

  async function processNext(): Promise<void> {
    if (processing || panelClosed) return;
    const next = queue.shift();
    if (next === undefined) return;
    processing = true;
    publishState();
    try {
      await runOne(next);
    } finally {
      processing = false;
      publishState();
      if (queue.length > 0 && !panelClosed) void processNext();
    }
  }

  return {
    enqueue(prompt: string): void {
      if (panelClosed) return;
      const trimmed = prompt.trim();
      if (!trimmed) return;
      queue.push(trimmed);
      publishState();
      void processNext();
    },
    interrupt(): void {
      // Abort only the current turn. The for-await loop in runOne
      // exits, the finally block clears `processing`, and
      // `processNext` advances to the next queued prompt (if any).
      currentTurnAc?.abort();
    },
    resetSession(): void {
      if (panelClosed) return;
      queue.length = 0;
      currentTurnAc?.abort();
      sessionUuid = randomUUID();
      hasStarted = false;
      publishState();
    },
    abort(): void {
      panelClosed = true;
      queue.length = 0;
      currentTurnAc?.abort();
      publishState();
    },
  };
}
