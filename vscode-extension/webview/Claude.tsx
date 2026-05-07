import * as React from 'react';
import { fileBasename, splitForFileRefs } from './fileRefs';
import {
  HistoryPicker,
  type SessionMetaLite,
} from './HistoryPicker';
import { MarkdownContent } from './MarkdownContent';
import { PermissionBubble } from './PermissionBubble';
import { processClaude, type ClaudeBlock } from './processClaude';
import { focusTextareaAt } from './textareaFocus';
import { ToolCard, shortenHookName } from './ToolCard';
import { useAutosize } from './useAutosize';
import { useStickyScroll } from './useStickyScroll';

export type SupportedPermissionMode =
  | 'bypassPermissions'
  | 'acceptEdits'
  | 'plan'
  | 'default';

export interface ClaudeRunnerState {
  busy: boolean;
  queued: number;
  mode: SupportedPermissionMode;
}

const MODE_LABEL: Record<SupportedPermissionMode, string> = {
  bypassPermissions: 'auto',
  acceptEdits: 'ask edits',
  plan: 'plan',
  default: 'ask all',
};

const MODE_DESCRIPTION: Record<SupportedPermissionMode, string> = {
  bypassPermissions:
    'Auto — every tool call runs without asking. Trusted Room default.',
  acceptEdits:
    'Ask before edits — Claude can read freely; Edit/Write/Bash prompts.',
  plan: 'Plan mode — Claude can read but cannot run any side-effectful tool.',
  default:
    'Ask all — every tool call shows an inline Allow/Deny bubble first.',
};

const MODE_ORDER: SupportedPermissionMode[] = [
  'bypassPermissions',
  'acceptEdits',
  'plan',
  'default',
];

// Re-export so existing `import { SessionMetaLite } from './Claude'`
// callsites (main.tsx) still resolve. The canonical definition now
// lives next to its primary consumer in HistoryPicker.tsx.
export type { SessionMetaLite } from './HistoryPicker';

interface ClaudeProps {
  events: unknown[];
  state: ClaudeRunnerState;
  onPrompt?: (body: string) => void;
  onInterrupt?: () => void;
  onResetSession?: () => void;
  onOpenFile?: (path: string) => void;
  onPermissionMode?: (mode: SupportedPermissionMode) => void;
  /** The VSCode editor's currently-active file. The Claude input
   *  shows a chip for it; click → insert `@<path>` into the draft. */
  activeEditor?: { path: string; basename: string } | null;
  /** Webview's reply to a `cc:permission-request` event. */
  onPermissionResponse?: (
    requestId: string,
    behavior: 'allow' | 'deny' | 'always-allow',
  ) => void;
  history?: {
    viewing?: string;
    sessions: SessionMetaLite[];
    onRequestList: () => void;
    onLoad: (sessionId: string) => void;
    onExit: () => void;
  };
}

export function Claude({
  events,
  state,
  onPrompt,
  onInterrupt,
  onResetSession,
  onOpenFile,
  onPermissionMode,
  onPermissionResponse,
  activeEditor,
  history,
}: ClaudeProps): React.ReactElement {
  const cyclePermissionMode = (): void => {
    if (!onPermissionMode) return;
    const idx = MODE_ORDER.indexOf(state.mode);
    const next = MODE_ORDER[(idx + 1) % MODE_ORDER.length];
    onPermissionMode(next);
  };

  const insertEditorRef = (): void => {
    if (!activeEditor) return;
    setDraft((prev) => {
      const ref = `@${activeEditor.path}`;
      // Already there → no-op so spamming the chip doesn't pile up
      // the same path five times.
      if (prev.includes(ref)) return prev;
      return prev ? `${prev.trimEnd()} ${ref} ` : `${ref} `;
    });
    focusTextareaAt(textareaRef);
  };
  const [historyOpen, setHistoryOpen] = React.useState(false);
  const viewingHistory = !!history?.viewing;
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
    // `tick` is intentionally in the deps so an in-flight thinking
    // block keeps re-deriving its elapsed value via processClaude.
    // Date.now() is static-call non-reactive — exhaustive-deps is
    // satisfied without a disable.
    [events, tick],
  );
  // Hide successful hook rows — pending and failed still surface.
  const visible = React.useMemo(
    () => blocks.filter((b) => !(b.kind === 'hook' && b.status === 'ok')),
    [blocks],
  );
  // Block input while a permission bubble is awaiting the user's
  // click — the next prompt would go ahead of the answer otherwise
  // and the in-flight tool call would stay frozen.
  const pendingPermissionCount = React.useMemo(
    () =>
      visible.reduce(
        (n, b) =>
          b.kind === 'permission' && b.status === 'pending' ? n + 1 : n,
        0,
      ),
    [visible],
  );
  const inputDisabled = pendingPermissionCount > 0;
  const scrollRef = useStickyScroll(visible.length);
  const [draft, setDraft] = React.useState('');
  const textareaRef = useAutosize(draft);

  React.useEffect(() => {
    textareaRef.current?.focus();
  }, [textareaRef]);

  const busyLabel = state.busy ? '· busy' : '';
  const placeholder = inputDisabled
    ? `Awaiting ${pendingPermissionCount} permission ${
        pendingPermissionCount === 1 ? 'decision' : 'decisions'
      }…`
    : state.busy
      ? 'Queue another message — Claude is working…'
      : 'Ask Claude — Enter to send · Shift+Enter for newline';

  const submit = (): void => {
    if (inputDisabled) return;
    const trimmed = draft.trim();
    if (!trimmed || !onPrompt) return;
    onPrompt(trimmed);
    setDraft('');
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    // IME composition (Pinyin, kana, etc.) — Enter finalizes the
    // candidate, never the prompt.
    if (e.nativeEvent.isComposing) return;
    if (e.key === 'Enter' && !e.shiftKey && !e.metaKey && !e.ctrlKey) {
      e.preventDefault();
      submit();
    }
  };

  const toggleHistory = (): void => {
    if (!history) return;
    const next = !historyOpen;
    setHistoryOpen(next);
    if (next) history.onRequestList();
  };

  return (
    <div className="pane">
      <div className="pane-head">
        <span>claude {busyLabel && <span className="pane-busy">{busyLabel}</span>}</span>
        <div className="pane-head-actions">
          {history && (
            <button
              type="button"
              className={`head-btn ${historyOpen ? 'active' : ''}`}
              onClick={toggleHistory}
              aria-label="History"
              title="Past Claude conversations in this workspace"
            >
              <i className="codicon codicon-history" />
            </button>
          )}
          {onResetSession && (
            <button
              type="button"
              className="head-btn"
              onClick={() => {
                setHistoryOpen(false);
                if (viewingHistory) history?.onExit();
                onResetSession();
              }}
              aria-label="New chat"
              title="New Claude session — clears history, fresh sessionId"
            >
              <i className="codicon codicon-add" />
            </button>
          )}
        </div>
      </div>
      {historyOpen && history && (
        <HistoryPicker
          sessions={history.sessions}
          viewing={history.viewing}
          onPick={(sid) => {
            setHistoryOpen(false);
            history.onLoad(sid);
          }}
          onClose={() => setHistoryOpen(false)}
        />
      )}
      {viewingHistory && (
        <div className="history-banner">
          <i className="codicon codicon-archive" />
          <span>viewing past conversation (read-only)</span>
          <button
            type="button"
            className="history-banner-exit"
            onClick={() => history?.onExit()}
          >
            return to live
          </button>
        </div>
      )}
      <div className="claude-log" ref={scrollRef}>
        {visible.length === 0 ? (
          <div className="muted">
            (idle — type a prompt below or @-mention from chat)
          </div>
        ) : (
          renderWithTurnSeparators(visible, onOpenFile, onPermissionResponse)
        )}
      </div>
      {!viewingHistory && state.queued > 0 && (
        <div className="queue-pill">
          {state.queued} queued · Claude is working on the previous prompt
        </div>
      )}
      {onPrompt && !viewingHistory && activeEditor && (
        <div className="pane-input-ref">
          <button
            type="button"
            className="editor-ref-chip"
            onClick={insertEditorRef}
            title={`Attach a reference to ${activeEditor.path}`}
          >
            <i className="codicon codicon-file" />
            <span>{activeEditor.basename}</span>
          </button>
          <span className="editor-ref-hint">active file · click to ref</span>
        </div>
      )}
      {onPrompt && !viewingHistory && (
        <div className="pane-input">
          <textarea
            ref={textareaRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder={placeholder}
            rows={1}
            disabled={inputDisabled}
          />
          {onPermissionMode && (
            <button
              type="button"
              className={`mode-pill mode-${state.mode}`}
              onClick={cyclePermissionMode}
              aria-label="Permission mode"
              title={MODE_DESCRIPTION[state.mode]}
            >
              <i className="codicon codicon-shield" />
              <span>{MODE_LABEL[state.mode]}</span>
            </button>
          )}
          {state.busy && onInterrupt ? (
            <button
              type="button"
              className="stop-btn"
              onClick={onInterrupt}
              aria-label="Stop"
              title="Stop the current turn"
            >
              <i className="codicon codicon-debug-stop" />
            </button>
          ) : (
            <button
              type="button"
              className="send-btn"
              onClick={submit}
              disabled={draft.trim().length === 0 || inputDisabled}
              aria-label="Send"
              title="Send (Enter)"
            >
              <i className="codicon codicon-send" />
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
// Only blocks that represent "Claude doing work" get the timeline
// bullet (tool calls + thinking + permission gate). Plain assistant
// text, user prompt echoes, session markers, results — those render
// flush so the conversation reads like a normal chat instead of a
// process trace.
function isTimelineBlock(b: ClaudeBlock): boolean {
  return (
    b.kind === 'tool' ||
    b.kind === 'thinking' ||
    b.kind === 'hook' ||
    b.kind === 'permission'
  );
}

function renderWithTurnSeparators(
  blocks: ClaudeBlock[],
  onOpenFile?: (path: string) => void,
  onPermissionResponse?: (
    requestId: string,
    behavior: 'allow' | 'deny' | 'always-allow',
  ) => void,
): React.ReactNode[] {
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
    const cls = isTimelineBlock(b)
      ? `claude-step ${stateClassFor(b)}`
      : `claude-flat ${stateClassFor(b)}`;
    out.push(
      <div key={i} className={cls}>
        <BlockRow
          block={b}
          onOpenFile={onOpenFile}
          onPermissionResponse={onPermissionResponse}
        />
      </div>,
    );
  }
  return out;
}

function stateClassFor(b: ClaudeBlock): string {
  switch (b.kind) {
    case 'session':
      return 'ok';
    case 'prompt':
      return 'me';
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
    case 'permission':
      if (b.status === 'pending') return 'pending';
      if (b.status === 'denied') return 'error';
      return 'ok'; // allowed | always-allowed
    case 'error':
      return 'error';
  }
}

function BlockRow({
  block,
  onOpenFile,
  onPermissionResponse,
}: {
  block: ClaudeBlock;
  onOpenFile?: (path: string) => void;
  onPermissionResponse?: (
    requestId: string,
    behavior: 'allow' | 'deny' | 'always-allow',
  ) => void;
}): React.ReactElement | null {
  switch (block.kind) {
    case 'session':
      return (
        <div className="claude-row claude-system">
          ▸ session ({block.sessionId.slice(0, 8)}…)
        </div>
      );
    case 'prompt':
      return (
        <div className="claude-row claude-prompt">
          <span className="claude-prompt-arrow">›</span>
          <span className="claude-prompt-body">
            <PromptText text={block.text} onOpenFile={onOpenFile} />
          </span>
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
    case 'permission':
      return (
        <PermissionBubble
          block={block}
          onRespond={onPermissionResponse}
        />
      );
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

function PromptText({
  text,
  onOpenFile,
}: {
  text: string;
  onOpenFile?: (path: string) => void;
}): React.ReactElement {
  const tokens = React.useMemo(() => splitForFileRefs(text), [text]);
  return (
    <>
      {tokens.map((tok, i) =>
        tok.kind === 'path' ? (
          <button
            key={i}
            type="button"
            className="file-chip"
            onClick={() => onOpenFile?.(tok.value)}
            title={tok.value}
          >
            <i className="codicon codicon-file" />
            <span className="file-chip-name">{fileBasename(tok.value)}</span>
          </button>
        ) : (
          <React.Fragment key={i}>{tok.value}</React.Fragment>
        ),
      )}
    </>
  );
}

