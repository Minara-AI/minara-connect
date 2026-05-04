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
//   - `abortController` for panel-dispose teardown
//
// Permission UI, mid-turn cancellation beyond panel-close, and
// MCP cc-connect server registration land in subsequent steps.

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
  enqueue(prompt: string): void;
  abort(): void;
}

export function createClaudeRunner(
  opts: ClaudeRunnerOptions,
): ClaudeRunnerHandle {
  const sessionUuid = randomUUID();
  const claudeBin = join(homedir(), '.local', 'bin', 'claude');
  const ac = new AbortController();
  let hasStarted = false;
  const queue: string[] = [];
  let processing = false;
  let aborted = false;

  function publishState(): void {
    opts.onStateChange({ busy: processing, queued: queue.length });
  }

  async function runOne(prompt: string): Promise<void> {
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
        // v0 trust posture: auto-allow every tool. The headless SDK
        // path has nowhere to surface a permission prompt — without
        // this, mcp__cc-connect__cc_at (Claude's only path back into
        // the Room) gets denied and Claude gives up on the round-
        // trip. Real per-tool approval UI in the Claude panel is
        // tracked as a §8 deferred item in the design doc.
        canUseTool: async () => ({ behavior: 'allow' as const }),
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
    }
  }

  async function processNext(): Promise<void> {
    if (processing || aborted) return;
    const next = queue.shift();
    if (next === undefined) return;
    processing = true;
    publishState();
    try {
      await runOne(next);
    } finally {
      processing = false;
      publishState();
      if (queue.length > 0 && !aborted) void processNext();
    }
  }

  return {
    enqueue(prompt: string): void {
      if (aborted) return;
      const trimmed = prompt.trim();
      if (!trimmed) return;
      queue.push(trimmed);
      publishState();
      void processNext();
    },
    abort(): void {
      aborted = true;
      queue.length = 0;
      ac.abort();
      publishState();
    },
  };
}
