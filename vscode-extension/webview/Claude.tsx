import * as React from 'react';
import { fileBasename, splitForFileRefs } from './fileRefs';
import { MarkdownContent } from './MarkdownContent';
import { processClaude, type ClaudeBlock } from './processClaude';
import {
  BashResultView,
  EditDiffView,
  ExpandableText,
} from './ToolCardBody';
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

export interface SessionMetaLite {
  sessionId: string;
  firstPrompt: string;
  mtimeMs: number;
  messageCount: number;
}

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
    requestAnimationFrame(() => {
      const ta = textareaRef.current;
      if (ta) {
        ta.focus();
        const end = ta.value.length;
        ta.setSelectionRange(end, end);
      }
    });
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
  const inputSummary = summarizeInput(block.name, block.input);
  const icon = iconForTool(short);
  return (
    <div className={cls}>
      <div className="claude-tool-head">
        <i className={`codicon codicon-${icon}`} />
        <span className="claude-tool-name">{short}</span>
        {!block.result && <span className="claude-tool-pending">⏳</span>}
      </div>
      <div className="claude-tool-block claude-tool-in">
        <span className="claude-tool-label">IN</span>
        <span className="claude-tool-input">{inputSummary || '(no args)'}</span>
      </div>
      <div className="claude-tool-block claude-tool-out">
        <span className="claude-tool-label">OUT</span>
        <div className="claude-tool-out-body">
          <ToolOutputView
            block={block}
            isEdit={isEdit}
            isBash={isBash}
          />
        </div>
      </div>
    </div>
  );
}

function ToolOutputView({
  block,
  isEdit,
  isBash,
}: {
  block: Extract<ClaudeBlock, { kind: 'tool' }>;
  isEdit: boolean;
  isBash: boolean;
}): React.ReactElement {
  if (isEdit) {
    return <EditDiffView input={block.input} />;
  }
  if (!block.result) {
    return <span className="claude-tool-pending-out">running…</span>;
  }
  const { fullText, isError } = block.result;
  if (!fullText) {
    return <span className="claude-tool-empty">(empty)</span>;
  }
  if (isBash) {
    return <BashResultView text={fullText} isError={isError} />;
  }
  return <ExpandableText text={fullText} isError={isError} />;
}


function HistoryPicker({
  sessions,
  viewing,
  onPick,
  onClose,
}: {
  sessions: SessionMetaLite[];
  viewing?: string;
  onPick: (sessionId: string) => void;
  onClose: () => void;
}): React.ReactElement {
  return (
    <div
      className="history-picker"
      role="dialog"
      aria-label="Past conversations"
    >
      <div className="history-picker-head">
        <span>past conversations</span>
        <button
          type="button"
          className="head-btn"
          onClick={onClose}
          aria-label="Close history"
          title="Close"
        >
          <i className="codicon codicon-close" />
        </button>
      </div>
      <div className="history-list">
        {sessions.length === 0 ? (
          <div className="muted">no past conversations in this workspace</div>
        ) : (
          sessions.map((s) => (
            <button
              key={s.sessionId}
              type="button"
              className={`history-item ${
                s.sessionId === viewing ? 'active' : ''
              }`}
              onClick={() => onPick(s.sessionId)}
              title={s.firstPrompt}
            >
              <div className="history-item-title">{s.firstPrompt}</div>
              <div className="history-item-meta">
                <span>{relativeTime(s.mtimeMs)}</span>
                <span>·</span>
                <span>{s.messageCount} msgs</span>
                <span>·</span>
                <span className="history-item-sid">
                  {s.sessionId.slice(0, 8)}
                </span>
              </div>
            </button>
          ))
        )}
      </div>
    </div>
  );
}

function relativeTime(ms: number): string {
  const delta = Date.now() - ms;
  const sec = Math.max(0, Math.floor(delta / 1000));
  if (sec < 60) return `${sec}s ago`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h ago`;
  const day = Math.floor(hr / 24);
  if (day < 7) return `${day}d ago`;
  return new Date(ms).toISOString().slice(0, 10);
}

function PermissionBubble({
  block,
  onRespond,
}: {
  block: Extract<ClaudeBlock, { kind: 'permission' }>;
  onRespond?: (
    requestId: string,
    behavior: 'allow' | 'deny' | 'always-allow',
  ) => void;
}): React.ReactElement {
  const headline =
    block.title ??
    `Claude wants to use ${block.toolName}${
      block.summary ? ` · ${block.summary}` : ''
    }`;
  const settled = block.status !== 'pending';
  const settledLabel =
    block.status === 'allowed'
      ? 'allowed'
      : block.status === 'always-allowed'
        ? 'always allowed'
        : block.status === 'denied'
          ? 'denied'
          : '';
  const tsLabel = block.ts
    ? new Date(block.ts).toTimeString().slice(0, 5)
    : '';
  return (
    <div className={`permission-bubble permission-${block.status}`}>
      <div className="permission-bubble-head">
        <i className="codicon codicon-shield" />
        <span className="permission-bubble-title">{headline}</span>
        {tsLabel && (
          <span className="permission-bubble-ts">{tsLabel}</span>
        )}
        {settled && (
          <span className="permission-bubble-state">{settledLabel}</span>
        )}
      </div>
      {block.description && (
        <div className="permission-bubble-desc">{block.description}</div>
      )}
      {block.blockedPath && (
        <div className="permission-bubble-meta">
          <span>blocked path:</span>
          <code>{block.blockedPath}</code>
        </div>
      )}
      {block.decisionReason && (
        <div className="permission-bubble-meta">
          <span>reason:</span>
          <code>{block.decisionReason}</code>
        </div>
      )}
      {block.summary && !block.title && (
        <div className="permission-bubble-summary">{block.summary}</div>
      )}
      {!settled && onRespond && (
        <div className="permission-bubble-actions">
          <button
            type="button"
            className="permission-btn permission-btn-deny"
            onClick={() => onRespond(block.requestId, 'deny')}
          >
            <i className="codicon codicon-circle-slash" />
            <span>Deny</span>
          </button>
          {block.canAlwaysAllow && (
            <button
              type="button"
              className="permission-btn permission-btn-always"
              onClick={() => onRespond(block.requestId, 'always-allow')}
              title="Add an SDK-suggested rule so this tool/input shape doesn't prompt again this session."
            >
              <i className="codicon codicon-shield" />
              <span>Always allow</span>
            </button>
          )}
          <button
            type="button"
            className="permission-btn permission-btn-allow"
            onClick={() => onRespond(block.requestId, 'allow')}
          >
            <i className="codicon codicon-check" />
            <span>Allow</span>
          </button>
        </div>
      )}
    </div>
  );
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

/** Pick a VSCode codicon for a given tool name so the tool card head
 *  reads at a glance (file = read/edit/write, terminal = bash, etc.).
 *  Anything unrecognised gets the generic "tools" icon. */
function iconForTool(short: string): string {
  switch (short) {
    case 'Read':
      return 'file-code';
    case 'Edit':
    case 'MultiEdit':
      return 'edit';
    case 'Write':
      return 'new-file';
    case 'Bash':
    case 'BashOutput':
      return 'terminal';
    case 'Grep':
      return 'search';
    case 'Glob':
      return 'file-submodule';
    case 'WebFetch':
    case 'WebSearch':
      return 'globe';
    case 'TodoWrite':
      return 'checklist';
    case 'Task':
    case 'Agent':
      return 'rocket';
    case 'cc_send':
    case 'cc_at':
      return 'comment';
    case 'cc_drop':
      return 'cloud-upload';
    case 'cc_recent':
    case 'cc_list_files':
      return 'list-unordered';
    case 'cc_wait_for_mention':
      return 'bell';
    case 'cc_save_summary':
      return 'note';
    default:
      return 'tools';
  }
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
