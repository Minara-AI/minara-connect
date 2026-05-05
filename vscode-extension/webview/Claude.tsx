import * as React from 'react';
import { MarkdownContent } from './MarkdownContent';
import { processClaude, type ClaudeBlock } from './processClaude';
import {
  BashResultView,
  EditDiffView,
  ExpandableText,
} from './ToolCardBody';
import { useAutosize } from './useAutosize';
import { useStickyScroll } from './useStickyScroll';

export interface ClaudeRunnerState {
  busy: boolean;
  queued: number;
}

interface ClaudeProps {
  events: unknown[];
  state: ClaudeRunnerState;
  onPrompt?: (body: string) => void;
  onInterrupt?: () => void;
  onResetSession?: () => void;
}

export function Claude({
  events,
  state,
  onPrompt,
  onInterrupt,
  onResetSession,
}: ClaudeProps): React.ReactElement {
  // Tick once a second while Claude is busy so the in-flight
  // "Thinking… Xs" block re-derives its elapsed value via processClaude.
  const [tick, setTick] = React.useState(0);
  React.useEffect(() => {
    if (!state.busy) return;
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, [state.busy]);

  const blocks = React.useMemo(
    () => processClaude(events, Date.now()),
    // Re-run when events change OR when the busy-tick advances so
    // an ongoing thinking block keeps updating.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [events, tick],
  );
  // Hide successful hook rows — pending and failed still surface.
  const visible = React.useMemo(
    () => blocks.filter((b) => !(b.kind === 'hook' && b.status === 'ok')),
    [blocks],
  );
  const scrollRef = useStickyScroll(visible.length);
  const [draft, setDraft] = React.useState('');
  const textareaRef = useAutosize(draft);

  React.useEffect(() => {
    textareaRef.current?.focus();
  }, [textareaRef]);

  const busyLabel = state.busy ? '· busy' : '';
  const placeholder = state.busy
    ? 'Queue another message — Claude is working…'
    : 'Ask Claude — Enter to send · Shift+Enter for newline';

  const submit = (): void => {
    const trimmed = draft.trim();
    if (!trimmed || !onPrompt) return;
    onPrompt(trimmed);
    setDraft('');
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    if (e.key === 'Enter' && !e.shiftKey && !e.metaKey && !e.ctrlKey) {
      e.preventDefault();
      submit();
    }
  };

  return (
    <div className="pane">
      <div className="pane-head">
        <span>claude {busyLabel && <span className="pane-busy">{busyLabel}</span>}</span>
        {onResetSession && (
          <button
            type="button"
            className="head-btn"
            onClick={onResetSession}
            aria-label="New chat"
            title="New Claude session — clears history, fresh sessionId"
          >
            <NewChatIcon />
          </button>
        )}
      </div>
      <div className="claude-log" ref={scrollRef}>
        {visible.length === 0 ? (
          <div className="muted">
            (idle — type a prompt below or @-mention from chat)
          </div>
        ) : (
          renderWithTurnSeparators(visible)
        )}
      </div>
      {state.queued > 0 && (
        <div className="queue-pill">
          {state.queued} queued · Claude is working on the previous prompt
        </div>
      )}
      {onPrompt && (
        <div className="pane-input">
          <textarea
            ref={textareaRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder={placeholder}
            rows={1}
          />
          {state.busy && onInterrupt ? (
            <button
              type="button"
              className="stop-btn"
              onClick={onInterrupt}
              aria-label="Stop"
              title="Stop the current turn"
            >
              <StopIcon />
            </button>
          ) : (
            <button
              type="button"
              className="send-btn"
              onClick={submit}
              disabled={draft.trim().length === 0}
              aria-label="Send"
              title="Send (Enter)"
            >
              <SendIcon />
            </button>
          )}
        </div>
      )}
    </div>
  );
}

/** Walks the block list and:
 *  - inserts a turn separator before each `session` event after the
 *    first one (each session block = a fresh `query()` call)
 *  - wraps every other block in a `<StepWrap>` that draws the
 *    vertical timeline + a state-colored bullet. */
function renderWithTurnSeparators(blocks: ClaudeBlock[]): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  let turn = 0;
  for (let i = 0; i < blocks.length; i++) {
    const b = blocks[i];
    if (b.kind === 'session') {
      turn += 1;
      if (turn > 1) {
        out.push(
          <div key={`sep-${i}`} className="claude-turn-sep">
            turn {turn}
          </div>,
        );
      }
    }
    out.push(
      <div key={i} className={`claude-step ${stateClassFor(b)}`}>
        <BlockRow block={b} />
      </div>,
    );
  }
  return out;
}

function stateClassFor(b: ClaudeBlock): string {
  switch (b.kind) {
    case 'session':
      return 'ok';
    case 'thinking':
      return b.ongoing ? 'pending' : 'ok';
    case 'text':
      return 'ok';
    case 'tool':
      if (!b.result) return 'pending';
      return b.result.isError ? 'error' : 'ok';
    case 'result':
      return b.isError ? 'error' : 'done';
    case 'hook':
      return b.status === 'pending'
        ? 'pending'
        : b.status === 'fail'
          ? 'error'
          : 'ok';
    case 'error':
      return 'error';
  }
}

function BlockRow({ block }: { block: ClaudeBlock }): React.ReactElement | null {
  switch (block.kind) {
    case 'session':
      return (
        <div className="claude-row claude-system">
          ▸ session ({block.sessionId.slice(0, 8)}…)
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
        block.status === 'fail'
          ? 'claude-row claude-hook claude-error'
          : 'claude-row claude-hook';
      return (
        <div className={cls}>
          {icon} {shortenHookName(block.hookName)}
          {block.status === 'fail' && block.exitCode !== undefined
            ? ` · exit ${block.exitCode}`
            : ''}
        </div>
      );
    }
    case 'thinking': {
      const secs = Math.max(1, Math.floor(block.elapsedMs / 1000));
      const label = block.ongoing
        ? `Thinking… ${secs}s`
        : `Thought for ${secs}s`;
      return (
        <div className={`claude-row claude-thinking${block.ongoing ? ' ongoing' : ''}`}>
          {label}
        </div>
      );
    }
    case 'text':
      return (
        <div className="claude-row claude-text">
          <MarkdownContent text={block.text} />
        </div>
      );
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
  const short = shortenToolName(block.name);
  const isEdit = short === 'Edit' || short === 'Write' || short === 'MultiEdit';
  const isBash = short === 'Bash';
  return (
    <div className={cls}>
      <div className="claude-tool-head">
        <span className="claude-tool-name">{short}</span>
        <span className="claude-tool-input">
          {summarizeInput(block.name, block.input)}
        </span>
        {!block.result && <span className="claude-tool-pending">⏳</span>}
      </div>
      {isEdit && <EditDiffView input={block.input} />}
      {block.result && (
        <ToolResultView
          isBash={isBash}
          fullText={block.result.fullText}
          isError={block.result.isError}
        />
      )}
    </div>
  );
}

function ToolResultView({
  isBash,
  fullText,
  isError,
}: {
  isBash: boolean;
  fullText: string;
  isError: boolean;
}): React.ReactElement {
  if (!fullText) {
    return <div className="claude-tool-empty">(empty)</div>;
  }
  if (isBash) {
    return <BashResultView text={fullText} isError={isError} />;
  }
  return <ExpandableText text={fullText} isError={isError} />;
}

function SendIcon(): React.ReactElement {
  return (
    <svg
      viewBox="0 0 16 16"
      width="14"
      height="14"
      fill="currentColor"
      aria-hidden="true"
    >
      <path d="M1.7 1.4a.6.6 0 0 1 .7-.05l11.7 6a.6.6 0 0 1 0 1.06l-11.7 6.1a.6.6 0 0 1-.86-.7l1.5-4.95a.6.6 0 0 1 .47-.42l5.34-.93a.2.2 0 0 0 0-.4l-5.34-.93a.6.6 0 0 1-.47-.42l-1.5-4.95a.6.6 0 0 1 .16-.6z" />
    </svg>
  );
}

function StopIcon(): React.ReactElement {
  return (
    <svg
      viewBox="0 0 16 16"
      width="12"
      height="12"
      fill="currentColor"
      aria-hidden="true"
    >
      <rect x="3" y="3" width="10" height="10" rx="1.5" />
    </svg>
  );
}

function NewChatIcon(): React.ReactElement {
  return (
    <svg
      viewBox="0 0 16 16"
      width="13"
      height="13"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M2.5 4.5h7M2.5 8h5M2.5 11.5h6" />
      <path d="M11 9.5v5M8.5 12h5" />
    </svg>
  );
}

function shortenToolName(name: string): string {
  const m = /^mcp__[^_]+(?:[^_]|_[^_])*?__(.+)$/.exec(name);
  return m ? m[1] : name;
}

function shortenHookName(name: string): string {
  const colon = name.indexOf(':');
  if (colon < 0) return name;
  const phase = name.slice(0, colon);
  const tool = shortenToolName(name.slice(colon + 1));
  return `${phase} · ${tool}`;
}

function summarizeInput(name: string, input: Record<string, unknown>): string {
  const short = shortenToolName(name);
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
