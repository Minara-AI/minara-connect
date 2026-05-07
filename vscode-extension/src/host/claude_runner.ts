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
import {
  query,
  type CanUseTool,
  type PermissionMode,
  type PermissionResult,
  type PermissionUpdate,
  type Query,
} from '@anthropic-ai/claude-agent-sdk';

export interface ClaudeRunnerState {
  busy: boolean;
  queued: number;
  mode: SupportedPermissionMode;
}

/** Subset we expose in the UI. `default` opts into per-tool approval —
 *  the runner installs a `canUseTool` callback that pauses each tool
 *  call until the webview's inline Allow/Deny bubble resolves. The
 *  SDK's `dontAsk` / `auto` are internals not meant for end-user
 *  toggling. */
export type SupportedPermissionMode =
  | 'bypassPermissions'
  | 'acceptEdits'
  | 'plan'
  | 'default';

/** Event the runner emits via onEvent when `default` mode triggers a
 *  per-tool approval. Webview renders an inline bubble; user reply
 *  travels back through the panel provider's `claude:permission-response`
 *  postMessage which calls `runner.resolvePermission(id, behavior)`. */
export interface PermissionRequestEvent {
  type: 'cc:permission-request';
  requestId: string;
  toolName: string;
  toolUseID: string;
  input: Record<string, unknown>;
  /** Sentence rendered by the SDK ("Claude wants to …"). Falls back
   *  to a synthesised one when missing. */
  title?: string;
  description?: string;
  blockedPath?: string;
  decisionReason?: string;
  /** Wall-clock ms when the SDK asked. The bubble renders an HH:MM
   *  stamp so users coming back to a paused turn can see how stale it
   *  is. */
  ts: number;
  /** True when the SDK supplied `suggestions` for the
   *  `always-allow this tool` shortcut — not every permission probe
   *  has them (path-blocked Bash calls usually do; raw MCP probes
   *  often don't). */
  canAlwaysAllow: boolean;
}

export interface PermissionResolveEvent {
  type: 'cc:permission-resolved';
  requestId: string;
  behavior: 'allow' | 'deny' | 'always-allow';
}

export type PermissionBehaviorChoice = 'allow' | 'deny' | 'always-allow';

export interface ClaudeRunnerOptions {
  topic: string;
  /** Appended to Claude's system prompt every turn — mirrors the
   *  `--append-system-prompt "$(cat auto-reply-prompt.md)"` flag the
   *  TUI passes through `claude-wrap.sh`. Tells Claude it's in a
   *  cc-connect Room + how to use the cc_* MCP tools. */
  systemPromptAppend?: string;
  /** First user prompt of a fresh Session. The TUI feeds Claude the
   *  contents of `bootstrap-prompt.md` here so it auto-greets the
   *  Room and enters the `cc_wait_for_mention` loop without the user
   *  having to type anything. */
  initialPrompt?: string;
  /** Persisted sessionId for this Room from a prior panel lifecycle.
   *  When set, the runner skips the auto-greet and uses `resume:` on
   *  the first turn so the conversation continues where it left off
   *  — rejoining a Room shouldn't re-broadcast hello. */
  resumeSessionId?: string;
  /** Notifies the panel of the runner's active sessionId so it can
   *  persist for next-rejoin resume. Fires once at construction
   *  (with the resumed or freshly-minted UUID) and again whenever
   *  `resetSession()` rotates it. */
  onSessionId?: (sessionId: string) => void;
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
  /** Switch permission mode. Applies to all subsequent turns; if a
   *  turn is in flight, also calls `query.setPermissionMode(mode)` so
   *  the live conversation flips immediately. */
  setPermissionMode(mode: SupportedPermissionMode): void;
  /** Webview's reply to a `cc:permission-request` event. No-op if the
   *  request id is unknown (timeout, runner reset, double-click, …).
   *  `always-allow` adds the SDK's suggested rules to the in-flight
   *  query so the same tool/input shape won't prompt again. */
  resolvePermission(
    requestId: string,
    behavior: PermissionBehaviorChoice,
  ): void;
  /** Tear the runner down: cancel current + clear queue. Used on
   *  panel dispose. */
  abort(): void;
}

export function createClaudeRunner(
  opts: ClaudeRunnerOptions,
): ClaudeRunnerHandle {
  // Resume a prior Session when the panel was reopened on the same
  // Room — `hasStarted` flips so the first turn uses `resume:` instead
  // of `sessionId:`, matching the post-first-turn path inside runOne.
  const resuming = !!opts.resumeSessionId;
  let sessionUuid = opts.resumeSessionId || randomUUID();
  const claudeBin = join(homedir(), '.local', 'bin', 'claude');
  let hasStarted = resuming;
  const queue: string[] = [];
  let processing = false;
  let panelClosed = false;
  let currentTurnAc: AbortController | null = null;
  // The active query handle, if a turn is in flight. Held so
  // setPermissionMode() can call `currentTurnQ.setPermissionMode(mode)`
  // and flip the in-progress conversation without aborting it.
  let currentTurnQ: Query | null = null;
  // Default mode mirrors the original v0 behaviour. The user can flip
  // via the UI pill — pure auto-bypass is the most common ergonomic
  // choice for cc-connect's "trusted Room" model.
  let currentMode: SupportedPermissionMode = 'bypassPermissions';

  // Pending permission requests keyed by requestId. Populated when
  // the SDK calls our canUseTool callback in `default` mode; resolved
  // when the webview's bubble click reaches resolvePermission(). On
  // runner abort / reset, every pending request is denied with
  // `interrupt: true` so the SDK breaks out of the in-flight tool call.
  const pendingPermissions = new Map<
    string,
    (result: PermissionResult) => void
  >();
  // SDK's `ctx.suggestions` per request — stored so `always-allow`
  // can echo them back as `updatedPermissions`. Cleared on resolve.
  const suggestionsByRequestId = new Map<string, PermissionUpdate[]>();
  let permissionSeq = 0;

  // Cleanup callbacks (mostly abort-listener removal) registered per
  // request, called from resolvePermission so we don't leak listeners
  // across many tool calls in one turn.
  const cleanupByRequestId = new Map<string, () => void>();

  const canUseTool: CanUseTool = (toolName, input, ctx) => {
    // The SDK can fire `canUseTool` mid-turn even after the user
    // toggled out of `default` mode (the new mode propagates on the
    // *next* tool call, not the in-flight one). Honor the new
    // posture immediately by auto-allowing — otherwise a ghost
    // permission bubble pops for an action the user already
    // implicitly authorised by switching modes.
    if (currentMode !== 'default') {
      return Promise.resolve({ behavior: 'allow' });
    }
    return new Promise<PermissionResult>((resolve) => {
      const requestId = `perm-${++permissionSeq}`;
      pendingPermissions.set(requestId, resolve);
      const suggestions = ctx.suggestions ?? [];
      if (suggestions.length > 0) {
        suggestionsByRequestId.set(requestId, suggestions);
      }

      // If the SDK aborts the operation (turn cancelled / runner
      // killed), resolve as a deny so the awaited promise unblocks.
      const onAbort = (): void => {
        const fn = pendingPermissions.get(requestId);
        if (fn) {
          pendingPermissions.delete(requestId);
          suggestionsByRequestId.delete(requestId);
          cleanupByRequestId.delete(requestId);
          fn({
            behavior: 'deny',
            message: 'permission request aborted',
            interrupt: true,
          });
        }
      };
      ctx.signal.addEventListener('abort', onAbort, { once: true });
      cleanupByRequestId.set(requestId, () => {
        ctx.signal.removeEventListener('abort', onAbort);
      });

      const event: PermissionRequestEvent = {
        type: 'cc:permission-request',
        requestId,
        toolName,
        toolUseID: ctx.toolUseID,
        input,
        title: ctx.title,
        description: ctx.description,
        blockedPath: ctx.blockedPath,
        decisionReason: ctx.decisionReason,
        ts: Date.now(),
        canAlwaysAllow: suggestions.length > 0,
      };
      try {
        opts.onEvent(event);
      } catch {
        // Webview unreachable — auto-deny so the turn doesn't hang.
        pendingPermissions.delete(requestId);
        suggestionsByRequestId.delete(requestId);
        cleanupByRequestId.get(requestId)?.();
        cleanupByRequestId.delete(requestId);
        resolve({
          behavior: 'deny',
          message: 'webview unreachable; cannot prompt for approval',
          interrupt: true,
        });
      }
    });
  };

  function denyAllPending(reason: string): void {
    for (const [requestId, fn] of pendingPermissions) {
      try {
        fn({ behavior: 'deny', message: reason, interrupt: true });
      } catch {
        /* swallow */
      }
      cleanupByRequestId.get(requestId)?.();
    }
    pendingPermissions.clear();
    suggestionsByRequestId.clear();
    cleanupByRequestId.clear();
  }

  // Auto-greet on Room join — mirrors the TUI's launcher-script path
  // (`claude-wrap.sh` invokes claude with bootstrap-prompt.md as the
  // first user message). We deliberately do NOT re-queue this on
  // `resetSession()` — that would re-broadcast a greeting to peers
  // every time the user clicks New chat, which is noisy.
  //
  // Same logic applies on resume: rejoining the Room should not
  // re-broadcast hello. The persisted Session continues silently and
  // wakes on the next @-mention or user prompt.
  //
  // `bootstrapPrompt` (when truthy) is the *one* turn that must
  // always run with bypassPermissions: the user didn't initiate it,
  // so popping a permission bubble for the auto-greet's `cc_send`
  // would be a confusing first-run UX. runOne checks identity to
  // decide whether to honor `currentMode` or force-bypass.
  const bootstrapPrompt = resuming ? '' : (opts.initialPrompt?.trim() || '');
  if (bootstrapPrompt) queue.push(bootstrapPrompt);

  // Hand the panel the sessionId we'll be using so it can persist for
  // next rejoin. Fires before any turn runs — even a runner that's
  // aborted before its first enqueue should leave a usable resume
  // pointer behind.
  try {
    opts.onSessionId?.(sessionUuid);
  } catch {
    /* swallow — best-effort persistence */
  }

  function publishState(): void {
    opts.onStateChange({
      busy: processing,
      queued: queue.length,
      mode: currentMode,
    });
  }

  async function runOne(prompt: string): Promise<void> {
    const ac = new AbortController();
    currentTurnAc = ac;
    const sessionOpt = hasStarted
      ? { resume: sessionUuid }
      : { sessionId: sessionUuid };
    const systemPromptOpt =
      opts.systemPromptAppend && opts.systemPromptAppend.trim()
        ? {
            systemPrompt: {
              type: 'preset' as const,
              preset: 'claude_code' as const,
              append: opts.systemPromptAppend,
            },
          }
        : {};
    // The auto-greet always runs with `bypassPermissions` regardless
    // of the UI pill — the user didn't initiate it, so a permission
    // bubble for `cc_send` would be a confusing first-run experience.
    // After the bootstrap turn, currentMode (the user's actual choice)
    // applies to every subsequent turn.
    const isBootstrap = !!bootstrapPrompt && prompt === bootstrapPrompt;
    const effectiveMode: SupportedPermissionMode = isBootstrap
      ? 'bypassPermissions'
      : currentMode;
    // Only attach canUseTool when the *effective* mode is `default`;
    // the bypassPermissions / acceptEdits / plan paths short-circuit
    // the callback at the SDK level, and the headless ZodError
    // pitfall is in *unconditional* canUseTool wiring.
    const permissionOpt: { canUseTool?: CanUseTool } =
      effectiveMode === 'default' ? { canUseTool } : {};
    const q = query({
      prompt,
      options: {
        ...sessionOpt,
        ...systemPromptOpt,
        ...permissionOpt,
        pathToClaudeCodeExecutable: claudeBin,
        includeHookEvents: true,
        abortController: ac,
        env: { ...process.env, CC_CONNECT_ROOM: opts.topic },
        // Effective mode for this turn — usually `currentMode`, but
        // forced to `bypassPermissions` for the bootstrap (auto-greet)
        // turn so peers don't see Claude blocked on a permission
        // dialog the user hasn't even seen yet.
        permissionMode: effectiveMode as PermissionMode,
      },
    });
    currentTurnQ = q;
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
      if (currentTurnQ === q) currentTurnQ = null;
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

  // Drain the bootstrap (if any) on the next tick. We can't call
  // processNext() inline here because the caller hasn't received the
  // handle yet — `onEvent` posts may race with the webview registering
  // listeners on the WebviewView. setTimeout(0) is enough.
  if (queue.length > 0) {
    setTimeout(() => {
      if (!panelClosed) void processNext();
    }, 0);
  }
  publishState();

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
    setPermissionMode(mode: SupportedPermissionMode): void {
      if (panelClosed) return;
      if (mode === currentMode) return;
      const wasDefault = currentMode === 'default';
      currentMode = mode;
      // Switching out of `default` while a permission request is
      // pending → auto-allow them. The user just chose a more
      // permissive posture; making them click each lingering bubble
      // would feel like the toggle didn't take.
      if (wasDefault && mode !== 'default' && pendingPermissions.size > 0) {
        for (const [requestId, fn] of pendingPermissions) {
          try {
            fn({ behavior: 'allow' });
          } catch {
            /* swallow */
          }
          cleanupByRequestId.get(requestId)?.();
        }
        pendingPermissions.clear();
        suggestionsByRequestId.clear();
        cleanupByRequestId.clear();
      }
      // Flip the in-flight conversation immediately if there is one.
      // Errors are swallowed: the next turn will pick up the new mode
      // anyway, so this is best-effort.
      const live = currentTurnQ;
      if (live) {
        void live.setPermissionMode(mode as PermissionMode).catch(() => {
          /* SDK may reject if the turn already finished — fine */
        });
      }
      publishState();
    },
    resolvePermission(
      requestId: string,
      behavior: PermissionBehaviorChoice,
    ): void {
      const fn = pendingPermissions.get(requestId);
      if (!fn) return;
      pendingPermissions.delete(requestId);
      const suggestions = suggestionsByRequestId.get(requestId);
      suggestionsByRequestId.delete(requestId);
      cleanupByRequestId.get(requestId)?.();
      cleanupByRequestId.delete(requestId);

      let result: PermissionResult;
      if (behavior === 'always-allow') {
        result = {
          behavior: 'allow',
          updatedPermissions: suggestions ?? [],
        };
      } else if (behavior === 'allow') {
        result = { behavior: 'allow' };
      } else {
        result = {
          behavior: 'deny',
          message: 'denied by user',
          interrupt: false,
        };
      }
      try {
        fn(result);
      } catch {
        /* swallow */
      }
      // Echo the resolution back to the webview so the bubble can
      // collapse / show its final state. Best-effort.
      const echo: PermissionResolveEvent = {
        type: 'cc:permission-resolved',
        requestId,
        behavior,
      };
      try {
        opts.onEvent(echo);
      } catch {
        /* swallow */
      }
    },
    resetSession(): void {
      if (panelClosed) return;
      queue.length = 0;
      currentTurnAc?.abort();
      denyAllPending('session reset');
      sessionUuid = randomUUID();
      hasStarted = false;
      try {
        opts.onSessionId?.(sessionUuid);
      } catch {
        /* swallow */
      }
      publishState();
    },
    abort(): void {
      panelClosed = true;
      queue.length = 0;
      currentTurnAc?.abort();
      denyAllPending('runner aborted');
      publishState();
    },
  };
}
