// Claude SDK event stream → renderable UI blocks.
//
// Architecture cribbed (with attribution) from sugyan/claude-code-webui's
// `UnifiedMessageProcessor` (MIT, frontend/src/utils/UnifiedMessageProcessor.ts):
// the same idea — cache tool_use by tool_use_id, correlate tool_result
// when it arrives, suppress noisy events like raw user-side tool_result
// envelopes, and collapse hook_started/hook_response pairs into a single
// row keyed by hook_id.
//
// Implemented from scratch for cc-connect — code is independent.

export type ClaudeBlock =
  | { kind: 'session'; sessionId: string }
  | { kind: 'hook'; hookId: string; hookName: string; status: 'pending' | 'ok' | 'fail'; exitCode?: number }
  | { kind: 'thinking'; elapsedMs: number; ongoing: boolean }
  | { kind: 'prompt'; text: string }
  | {
      kind: 'permission';
      requestId: string;
      toolName: string;
      summary: string;
      title?: string;
      description?: string;
      blockedPath?: string;
      decisionReason?: string;
      ts?: number;
      canAlwaysAllow?: boolean;
      status: 'pending' | 'allowed' | 'denied' | 'always-allowed';
    }
  | { kind: 'text'; text: string }
  | {
      kind: 'tool';
      toolUseId: string;
      name: string;
      input: Record<string, unknown>;
      result?: {
        fullText: string;
        truncated: boolean;
        isError: boolean;
      };
    }
  | {
      kind: 'result';
      numTurns: number;
      costUsd?: number;
      isError?: boolean;
      errorText?: string;
    }
  | { kind: 'error'; message: string };

interface RawEvent {
  type?: string;
  subtype?: string;
  session_id?: string;
  hook_id?: string;
  hook_name?: string;
  exit_code?: number;
  outcome?: string;
  num_turns?: number;
  total_cost_usd?: number;
  is_error?: boolean;
  result?: string;
  api_error_status?: string | null;
  error?: string;
  message?: {
    content?: ContentBlock[] | string;
  };
  /** Stamped by the webview on arrival — used to derive
   *  "Thought for Xs" between session start and first content. */
  _receivedAt?: number;
}

/** Don't render a thinking block for sub-second turns — too jittery. */
const THINKING_MIN_MS = 1500;

interface ContentBlock {
  type?: string;
  text?: string;
  name?: string;
  input?: Record<string, unknown>;
  tool_use_id?: string;
  content?: string | Array<{ type?: string; text?: string }>;
  is_error?: boolean;
}

// Cap stored tool-result text at 8 KB. Anything bigger gets a marker
// appended; the user expands via the UI if they need to see all of it
// (full content lives in claude's own transcript JSONL anyway).
const RESULT_FULL_CAP = 8000;

export function processClaude(
  events: unknown[],
  now: number = Date.now(),
): ClaudeBlock[] {
  const blocks: ClaudeBlock[] = [];
  const hookIdxById = new Map<string, number>();
  const toolIdxByUseId = new Map<string, number>();
  const permissionIdxByReqId = new Map<string, number>();

  // Per-turn thinking tracker: when a `system:init` lands we record
  // its wall-clock arrival; when the first text/tool of that turn
  // arrives we insert a `thinking` block ahead of it with the gap.
  // If the turn is still pending at the end of the event list, an
  // `ongoing: true` thinking block is appended so the UI shows a
  // live "Thinking… Xs" counter.
  let turnStartedAt: number | null = null;
  let turnHasContent = false;

  const maybeInsertThinking = (atTs: number): void => {
    if (turnStartedAt === null || turnHasContent) return;
    const elapsed = atTs - turnStartedAt;
    if (elapsed >= THINKING_MIN_MS) {
      blocks.push({ kind: 'thinking', elapsedMs: elapsed, ongoing: false });
    }
    turnHasContent = true;
  };

  for (const raw of events) {
    const ev = raw as RawEvent & { body?: string };
    const t = ev.type;
    const sub = ev.subtype;

    if (t === 'cc:user-prompt' && typeof ev.body === 'string') {
      blocks.push({ kind: 'prompt', text: ev.body });
      continue;
    }

    if (t === 'cc:permission-request') {
      const r = raw as {
        requestId?: string;
        toolName?: string;
        toolUseID?: string;
        input?: Record<string, unknown>;
        title?: string;
        description?: string;
        blockedPath?: string;
        decisionReason?: string;
        ts?: number;
        canAlwaysAllow?: boolean;
      };
      if (typeof r.requestId === 'string' && typeof r.toolName === 'string') {
        const summary = summariseToolInput(r.toolName, r.input ?? {});
        blocks.push({
          kind: 'permission',
          requestId: r.requestId,
          toolName: r.toolName,
          summary,
          title: r.title,
          description: r.description,
          blockedPath: r.blockedPath,
          decisionReason: r.decisionReason,
          ts: r.ts,
          canAlwaysAllow: r.canAlwaysAllow,
          status: 'pending',
        });
        permissionIdxByReqId.set(r.requestId, blocks.length - 1);
      }
      continue;
    }

    if (t === 'cc:permission-resolved') {
      const r = raw as {
        requestId?: string;
        behavior?: 'allow' | 'deny' | 'always-allow';
      };
      if (typeof r.requestId === 'string') {
        const idx = permissionIdxByReqId.get(r.requestId);
        if (idx !== undefined) {
          const prev = blocks[idx];
          if (prev?.kind === 'permission') {
            const status =
              r.behavior === 'always-allow'
                ? 'always-allowed'
                : r.behavior === 'allow'
                  ? 'allowed'
                  : 'denied';
            blocks[idx] = { ...prev, status };
          }
        }
      }
      continue;
    }

    if (t === 'system' && sub === 'init' && ev.session_id) {
      turnStartedAt = ev._receivedAt ?? now;
      turnHasContent = false;
      blocks.push({ kind: 'session', sessionId: ev.session_id });
      continue;
    }

    if (t === 'system' && sub === 'hook_started' && ev.hook_id) {
      blocks.push({
        kind: 'hook',
        hookId: ev.hook_id,
        hookName: ev.hook_name ?? '?',
        status: 'pending',
      });
      hookIdxById.set(ev.hook_id, blocks.length - 1);
      continue;
    }

    if (t === 'system' && sub === 'hook_response' && ev.hook_id) {
      const idx = hookIdxById.get(ev.hook_id);
      const ok = ev.outcome === 'success' && (ev.exit_code ?? 0) === 0;
      if (idx !== undefined) {
        const prev = blocks[idx];
        if (prev?.kind === 'hook') {
          blocks[idx] = {
            ...prev,
            status: ok ? 'ok' : 'fail',
            exitCode: ev.exit_code,
          };
        }
      } else {
        blocks.push({
          kind: 'hook',
          hookId: ev.hook_id,
          hookName: ev.hook_name ?? '?',
          status: ok ? 'ok' : 'fail',
          exitCode: ev.exit_code,
        });
      }
      continue;
    }

    if (t === 'assistant' && Array.isArray(ev.message?.content)) {
      // First text/tool of this turn → insert thinking block before
      // the content if the gap is meaningful.
      maybeInsertThinking(ev._receivedAt ?? now);
      for (const block of ev.message.content) {
        if (block.type === 'text' && typeof block.text === 'string') {
          blocks.push({ kind: 'text', text: block.text });
        } else if (block.type === 'tool_use' && block.name) {
          const useId = (block as { id?: string }).id ?? `tool-${blocks.length}`;
          blocks.push({
            kind: 'tool',
            toolUseId: useId,
            name: block.name,
            input: block.input ?? {},
          });
          toolIdxByUseId.set(useId, blocks.length - 1);
        }
      }
      continue;
    }

    if (t === 'user' && typeof ev.message?.content === 'string') {
      // Replayed transcript: the user side stores prompts as plain
      // strings, not as a `cc:user-prompt` synthetic event. Strip
      // common system-injected wrappers before rendering — these are
      // generated by Claude Code itself, not the human.
      const cleaned = stripPromptWrappers(ev.message.content);
      if (cleaned) blocks.push({ kind: 'prompt', text: cleaned });
      continue;
    }

    if (t === 'user' && Array.isArray(ev.message?.content)) {
      // Look for tool_result blocks; attach to the matching tool card.
      // Suppress everything else (these are MCP/tool-result envelopes
      // that Claude sees but the user doesn't care about).
      for (const block of ev.message.content) {
        if (block.type !== 'tool_result' || !block.tool_use_id) continue;
        const idx = toolIdxByUseId.get(block.tool_use_id);
        const raw = stringifyContent(block.content);
        const truncated = raw.length > RESULT_FULL_CAP;
        const fullText = truncated
          ? raw.slice(0, RESULT_FULL_CAP) +
            `\n\n… [truncated, ${raw.length - RESULT_FULL_CAP} more chars]`
          : raw;
        if (idx !== undefined) {
          const prev = blocks[idx];
          if (prev?.kind === 'tool') {
            blocks[idx] = {
              ...prev,
              result: {
                fullText,
                truncated,
                isError: !!block.is_error,
              },
            };
          }
        }
      }
      continue;
    }

    if (t === 'result') {
      // Result implicitly closes the turn; clear thinking state so we
      // don't try to insert a thinking row for a turn that ended
      // (e.g. an error result with no text).
      turnStartedAt = null;
      turnHasContent = true;
      const isError = !!ev.is_error || (sub !== undefined && sub !== 'success');
      const errorText = isError
        ? (ev.result?.trim() || ev.api_error_status || sub || 'unknown error')
        : undefined;
      blocks.push({
        kind: 'result',
        numTurns: ev.num_turns ?? 0,
        costUsd: ev.total_cost_usd,
        isError,
        errorText,
      });
      continue;
    }

    if (t === 'sdk:error') {
      blocks.push({ kind: 'error', message: ev.error ?? 'sdk error' });
      continue;
    }

    // Suppress everything else (rate_limit_event, plain user prompts, etc.).
  }

  // In-flight thinking: turn started, no content yet. Render an
  // ongoing block at the tail; Claude.tsx ticks `now` so the elapsed
  // count updates live.
  if (turnStartedAt !== null && !turnHasContent) {
    const elapsed = now - turnStartedAt;
    if (elapsed >= THINKING_MIN_MS) {
      blocks.push({ kind: 'thinking', elapsedMs: elapsed, ongoing: true });
    }
  }

  return blocks;
}

/** Drop the leading `<local-command-caveat>…</…>`, `<ide_opened_file>
 *  …</…>`, `<command-name>…</…>`, `<system-reminder>…</…>` and similar
 *  wrappers Claude Code injects ahead of real user prompts in
 *  transcript JSONL. Mirrors `transcripts.ts::stripSystemWrappers`. */
function stripPromptWrappers(raw: string): string {
  let s = raw.trim();
  for (let i = 0; i < 8; i++) {
    const m = /^<([a-z][a-z0-9_-]*)[^>]*>[\s\S]*?<\/\1>\s*/i.exec(s);
    if (!m) break;
    s = s.slice(m[0].length);
  }
  return s.trim();
}

/** Compact human-readable rendering of a tool's input for the
 *  permission bubble. We mirror the heuristic used in Claude.tsx for
 *  ToolCard but at a much shorter character cap — the bubble is a
 *  small inline UI, not a full card. */
function summariseToolInput(
  toolName: string,
  input: Record<string, unknown>,
): string {
  const candidates: Record<string, string[]> = {
    Read: ['file_path', 'path'],
    Edit: ['file_path', 'path'],
    Write: ['file_path', 'path'],
    Bash: ['command'],
    Grep: ['pattern'],
    Glob: ['pattern'],
  };
  const short = /^mcp__[^_]+(?:[^_]|_[^_])*?__(.+)$/.exec(toolName)?.[1] ?? toolName;
  const keys = candidates[short] ?? Object.keys(input);
  const parts: string[] = [];
  for (const k of keys.slice(0, 1)) {
    const v = input[k];
    if (v === undefined) continue;
    const s = typeof v === 'string' ? v : JSON.stringify(v);
    parts.push(s.length > 80 ? s.slice(0, 77) + '…' : s);
  }
  return parts.join(' · ');
}

function stringifyContent(
  content: string | Array<{ type?: string; text?: string }> | undefined,
): string {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map((c) => (c.type === 'text' ? (c.text ?? '') : ''))
      .filter(Boolean)
      .join('\n');
  }
  return '';
}
