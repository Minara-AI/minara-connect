import * as React from 'react';
import { processClaude, type ClaudeBlock } from './processClaude';
import { useStickyScroll } from './useStickyScroll';

export interface ClaudeRunnerState {
  busy: boolean;
  queued: number;
}

interface ClaudeProps {
  events: unknown[];
  state: ClaudeRunnerState;
}

export function Claude({ events, state }: ClaudeProps): React.ReactElement {
  const blocks = React.useMemo(() => processClaude(events), [events]);
  // Hide successful hook rows — they're nearly all the noise. Pending
  // (transient) and failed hooks still surface so the user can see
  // when something's actually wrong with the hook stack.
  const visible = React.useMemo(
    () => blocks.filter((b) => !(b.kind === 'hook' && b.status === 'ok')),
    [blocks],
  );
  const scrollRef = useStickyScroll(visible.length);
  const busyLabel = state.busy
    ? state.queued > 0
      ? `· busy (${state.queued} queued)`
      : '· busy'
    : '';
  return (
    <div className="pane">
      <h2>
        claude {busyLabel && <span className="pane-busy">{busyLabel}</span>}
      </h2>
      <div className="claude-log" ref={scrollRef}>
        {visible.length === 0 ? (
          <div className="muted">(idle — @-mention me from chat to start)</div>
        ) : (
          visible.map((b, i) => <BlockRow key={i} block={b} />)
        )}
      </div>
    </div>
  );
}

function BlockRow({ block }: { block: ClaudeBlock }): React.ReactElement | null {
  switch (block.kind) {
    case 'session':
      return (
        <div className="claude-row claude-system">
          ▸ session start ({block.sessionId.slice(0, 8)}…)
        </div>
      );
    case 'hook': {
      const icon =
        block.status === 'pending'
          ? '⏳'
          : block.status === 'ok'
            ? '·'
            : '✗';
      const cls =
        block.status === 'fail' ? 'claude-row claude-hook claude-error' : 'claude-row claude-hook';
      return (
        <div className={cls}>
          {icon} {shortenHookName(block.hookName)}
          {block.status === 'fail' && block.exitCode !== undefined
            ? ` · exit ${block.exitCode}`
            : ''}
        </div>
      );
    }
    case 'text':
      return <div className="claude-row claude-text">{block.text}</div>;
    case 'tool':
      return <ToolCard block={block} />;
    case 'result': {
      const cost =
        typeof block.costUsd === 'number'
          ? ` · $${block.costUsd.toFixed(3)}`
          : '';
      if (block.isError) {
        return (
          <div className="claude-row claude-result claude-error">
            ✗ {block.errorText ?? 'failed'} ({block.numTurns} turn
            {block.numTurns === 1 ? '' : 's'}
            {cost})
          </div>
        );
      }
      return (
        <div className="claude-row claude-result">
          ✓ done ({block.numTurns} turn{block.numTurns === 1 ? '' : 's'}
          {cost})
        </div>
      );
    }
    case 'error':
      return <div className="claude-row claude-error">✗ {block.message}</div>;
  }
}

function ToolCard({
  block,
}: {
  block: Extract<ClaudeBlock, { kind: 'tool' }>;
}): React.ReactElement {
  const cls = block.result?.isError
    ? 'claude-tool-card claude-tool-error'
    : 'claude-tool-card';
  return (
    <div className={cls}>
      <div className="claude-tool-head">
        <span className="claude-tool-name">{shortenToolName(block.name)}</span>
        <span className="claude-tool-input">{summarizeInput(block.name, block.input)}</span>
      </div>
      {block.result && (
        <div className="claude-tool-result">
          {block.result.isError ? '✗ ' : '↳ '}
          {block.result.preview || '(empty)'}
        </div>
      )}
    </div>
  );
}

/** Strip the `mcp__<server>__` prefix off MCP tool names so they
 *  render as e.g. `cc_at` instead of `mcp__cc-connect__cc_at`. Native
 *  tools like Read / Edit / Bash pass through unchanged. */
function shortenToolName(name: string): string {
  const m = /^mcp__[^_]+(?:[^_]|_[^_])*?__(.+)$/.exec(name);
  return m ? m[1] : name;
}

/** `PreToolUse:mcp__cc-connect__cc_send` → `PreToolUse · cc_send` */
function shortenHookName(name: string): string {
  const colon = name.indexOf(':');
  if (colon < 0) return name;
  const phase = name.slice(0, colon);
  const tool = shortenToolName(name.slice(colon + 1));
  return `${phase} · ${tool}`;
}

/** Pick the most useful field from common tools' input shape. */
function summarizeInput(name: string, input: Record<string, unknown>): string {
  const short = shortenToolName(name);
  // Tool-specific best-fit field.
  const candidates: Record<string, string[]> = {
    Read: ['file_path', 'path'],
    Edit: ['file_path', 'path'],
    Write: ['file_path', 'path'],
    Bash: ['command'],
    Grep: ['pattern'],
    Glob: ['pattern'],
    cc_send: ['body'],
    cc_at: ['nick', 'body'],
    cc_drop: ['path'],
    cc_recent: ['limit'],
    cc_save_summary: ['body'],
    cc_wait_for_mention: ['timeout_seconds'],
  };
  const keys = candidates[short] ?? Object.keys(input);
  const parts: string[] = [];
  for (const k of keys.slice(0, 2)) {
    const v = input[k];
    if (v === undefined) continue;
    const s = typeof v === 'string' ? v : JSON.stringify(v);
    parts.push(s.length > 60 ? s.slice(0, 57) + '…' : s);
  }
  return parts.join(' · ');
}
