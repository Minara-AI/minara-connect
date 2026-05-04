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
  | { kind: 'text'; text: string }
  | {
      kind: 'tool';
      toolUseId: string;
      name: string;
      input: Record<string, unknown>;
      result?: { preview: string; truncated: boolean; isError: boolean };
    }
  | { kind: 'result'; numTurns: number; costUsd?: number }
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
  error?: string;
  message?: {
    content?: ContentBlock[] | string;
  };
}

interface ContentBlock {
  type?: string;
  text?: string;
  name?: string;
  input?: Record<string, unknown>;
  tool_use_id?: string;
  content?: string | Array<{ type?: string; text?: string }>;
  is_error?: boolean;
}

const RESULT_PREVIEW_LIMIT = 200;

export function processClaude(events: unknown[]): ClaudeBlock[] {
  const blocks: ClaudeBlock[] = [];
  const hookIdxById = new Map<string, number>();
  const toolIdxByUseId = new Map<string, number>();

  for (const raw of events) {
    const ev = raw as RawEvent;
    const t = ev.type;
    const sub = ev.subtype;

    if (t === 'system' && sub === 'init' && ev.session_id) {
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

    if (t === 'user' && Array.isArray(ev.message?.content)) {
      // Look for tool_result blocks; attach to the matching tool card.
      // Suppress everything else (these are MCP/tool-result envelopes
      // that Claude sees but the user doesn't care about).
      for (const block of ev.message.content) {
        if (block.type !== 'tool_result' || !block.tool_use_id) continue;
        const idx = toolIdxByUseId.get(block.tool_use_id);
        const text = stringifyContent(block.content);
        const truncated = text.length > RESULT_PREVIEW_LIMIT;
        const preview = truncated
          ? text.slice(0, RESULT_PREVIEW_LIMIT) + '…'
          : text;
        if (idx !== undefined) {
          const prev = blocks[idx];
          if (prev?.kind === 'tool') {
            blocks[idx] = {
              ...prev,
              result: { preview, truncated, isError: !!block.is_error },
            };
          }
        }
      }
      continue;
    }

    if (t === 'result') {
      blocks.push({
        kind: 'result',
        numTurns: ev.num_turns ?? 0,
        costUsd: ev.total_cost_usd,
      });
      continue;
    }

    if (t === 'sdk:error') {
      blocks.push({ kind: 'error', message: ev.error ?? 'sdk error' });
      continue;
    }

    // Suppress everything else (rate_limit_event, plain user prompts, etc.).
  }

  return blocks;
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
