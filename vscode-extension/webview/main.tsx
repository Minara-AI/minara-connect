import * as React from 'react';
import { createRoot } from 'react-dom/client';
import { Chat } from './Chat';
import {
  Claude,
  type ClaudeRunnerState,
  type SessionMetaLite,
  type SupportedPermissionMode,
} from './Claude';
import type { Message } from './types';

declare global {
  interface Window {
    acquireVsCodeApi(): {
      postMessage(msg: unknown): void;
      setState(state: unknown): void;
      getState(): unknown;
    };
  }
}

const vscode = window.acquireVsCodeApi();

// Capture every host→webview message starting at script load, BEFORE
// React mounts. VSCode buffers postMessage()s the host sent while the
// webview was still loading, then dispatches them at script-execute
// time — that fires *before* React's useEffect attaches its listener,
// so an early backfill (Room messages, room:state, claude history)
// would otherwise be dropped silently. The component drains this
// buffer in its first effect.
const earlyMessages: MessageEvent[] = [];
let earlyHandler: ((e: MessageEvent) => void) | null = (e: MessageEvent) => {
  earlyMessages.push(e);
};
window.addEventListener('message', earlyHandler);

type Tab = 'chat' | 'claude';

function App(): React.ReactElement {
  const [status, setStatus] = React.useState('waiting…');
  const [messages, setMessages] = React.useState<Message[]>([]);
  const [myNick, setMyNick] = React.useState('(me)');
  const [topic, setTopic] = React.useState('');
  const [claudeEvents, setClaudeEvents] = React.useState<unknown[]>([]);
  const [claudeState, setClaudeState] = React.useState<ClaudeRunnerState>({
    busy: false,
    queued: 0,
    mode: 'bypassPermissions',
  });
  const [activeTab, setActiveTab] = React.useState<Tab>('chat');
  const [chatUnread, setChatUnread] = React.useState(0);
  const [claudeUnread, setClaudeUnread] = React.useState(0);
  const [historySessions, setHistorySessions] = React.useState<
    SessionMetaLite[]
  >([]);
  const [activeEditor, setActiveEditor] = React.useState<{
    path: string;
    basename: string;
  } | null>(null);
  // Live event stream during normal use; replaced by transcript
  // events when the user picks a past conversation. Reset to live
  // when they hit "return to live" or "new chat".
  const [viewingSessionId, setViewingSessionId] = React.useState<
    string | undefined
  >(undefined);
  const liveEventsRef = React.useRef<unknown[]>([]);
  // Track active tab via ref so async message handlers see the
  // current value without re-binding the listener on each switch.
  const activeTabRef = React.useRef(activeTab);
  React.useEffect(() => {
    activeTabRef.current = activeTab;
    if (activeTab === 'chat') setChatUnread(0);
    if (activeTab === 'claude') setClaudeUnread(0);
  }, [activeTab]);

  const viewingRef = React.useRef<string | undefined>(undefined);
  React.useEffect(() => {
    viewingRef.current = viewingSessionId;
  }, [viewingSessionId]);

  React.useEffect(() => {
    const onMsg = (event: MessageEvent): void => {
      const msg = (event.data ?? {}) as { type?: string; body?: unknown };
      if (msg.type === 'host:ready') {
        setStatus('ready');
      } else if (msg.type === 'room:reset') {
        setMessages([]);
        setClaudeEvents([]);
        liveEventsRef.current = [];
        setClaudeState((prev) => ({ ...prev, busy: false, queued: 0 }));
        setTopic('');
        setStatus('switching…');
        setChatUnread(0);
        setClaudeUnread(0);
        setHistorySessions([]);
        setViewingSessionId(undefined);
      } else if (msg.type === 'room:state') {
        const b = (msg.body ?? {}) as { topic?: string; myNick?: string };
        if (b.topic) setTopic(b.topic);
        if (b.myNick) setMyNick(b.myNick);
      } else if (msg.type === 'chat:message') {
        const m = msg.body as Message;
        setMessages((prev) => {
          if (prev.some((x) => x.id === m.id)) return prev;
          return [...prev, m].sort((a, b) => a.id.localeCompare(b.id));
        });
        if (activeTabRef.current !== 'chat') {
          setChatUnread((n) => n + 1);
        }
      } else if (msg.type === 'chat:send-error') {
        setStatus(`send failed: ${String(msg.body)}`);
      } else if (msg.type === 'claude:event') {
        const stamped =
          msg.body && typeof msg.body === 'object'
            ? { ...(msg.body as object), _receivedAt: Date.now() }
            : msg.body;
        // Mutate the ref-array (O(1) push) instead of cloning. A
        // long turn can fire 200+ events; the prior `[...prev, x]`
        // pattern is O(n²) and shows up as jank on busy turns.
        // Slice when handing to React state to keep the immutable
        // contract for downstream useMemo keyed on `events`.
        liveEventsRef.current.push(stamped);
        if (!viewingRef.current) {
          setClaudeEvents(liveEventsRef.current.slice());
        }
        if (activeTabRef.current !== 'claude') {
          // Only count assistant-text-bearing events as "unread"; the
          // hook/system spam is too noisy to surface in the badge.
          const ev = msg.body as { type?: string };
          if (ev?.type === 'assistant') {
            setClaudeUnread((n) => n + 1);
          }
        }
      } else if (msg.type === 'claude:state') {
        const s = msg.body as ClaudeRunnerState;
        setClaudeState((prev) => ({
          busy: !!s.busy,
          queued: s.queued ?? 0,
          mode: s.mode ?? prev.mode,
        }));
      } else if (msg.type === 'editor:active') {
        const b = msg.body as
          | { path: string; basename: string }
          | null
          | undefined;
        setActiveEditor(b ?? null);
      } else if (msg.type === 'history:list-result') {
        const list = (msg.body ?? []) as SessionMetaLite[];
        setHistorySessions(list);
      } else if (msg.type === 'history:loaded') {
        const b = msg.body as {
          sessionId: string;
          events: unknown[];
          error?: string;
        };
        if (b.error) {
          setStatus(`history load failed: ${b.error}`);
          return;
        }
        setViewingSessionId(b.sessionId);
        setClaudeEvents(b.events);
      }
    };
    // Order matters here: attach the real listener FIRST, so any
    // host message that arrives mid-handover lands on it. Then
    // detach the early-handler. Then drain the buffer atomically
    // (splice() empties + returns; safe to call repeatedly across
    // re-mounts). Without this order, a message that arrived
    // between the handover steps would vanish silently — exactly
    // the bug the buffer was added to fix.
    window.addEventListener('message', onMsg);
    if (earlyHandler) {
      window.removeEventListener('message', earlyHandler);
      earlyHandler = null;
    }
    const drained = earlyMessages.splice(0);
    for (const buffered of drained) onMsg(buffered);
    return () => window.removeEventListener('message', onMsg);
  }, []);

  const onSend = (body: string): void => {
    vscode.postMessage({ type: 'chat:send', body });
  };

  const onAttach = (): void => {
    vscode.postMessage({ type: 'chat:attach' });
  };

  const onPasteFiles = (
    files: { name: string; dataB64: string }[],
  ): void => {
    if (files.length === 0) return;
    vscode.postMessage({ type: 'chat:paste-files', body: files });
  };

  const onOpenDrop = (filename: string): void => {
    vscode.postMessage({ type: 'chat:open-drop', body: filename });
  };

  const onSaveDrop = (filename: string): void => {
    vscode.postMessage({ type: 'chat:save-drop', body: filename });
  };

  const onPrompt = (body: string): void => {
    vscode.postMessage({ type: 'claude:prompt', body });
    // Synthesize a local-only event so the prompt renders in the
    // Claude log immediately. SDK events stream back without echoing
    // the user side, and we want chips + scrollback parity.
    // Field is `text` (not `body`) to avoid colliding with the outer
    // postMessage envelope's `body` field — different layer, separate
    // shape.
    const synthetic = {
      type: 'cc:user-prompt',
      text: body,
      _receivedAt: Date.now(),
    };
    liveEventsRef.current.push(synthetic);
    setClaudeEvents(liveEventsRef.current.slice());
  };

  const onOpenFile = (path: string): void => {
    vscode.postMessage({ type: 'prompt:open-file', body: path });
  };

  const onRequestHistoryList = (): void => {
    vscode.postMessage({ type: 'history:list' });
  };

  const onLoadHistory = (sessionId: string): void => {
    vscode.postMessage({ type: 'history:load', body: sessionId });
  };

  const onExitHistory = (): void => {
    setViewingSessionId(undefined);
    setClaudeEvents(liveEventsRef.current.slice());
  };

  const onInterrupt = (): void => {
    vscode.postMessage({ type: 'claude:interrupt' });
  };

  const onResetSession = (): void => {
    setClaudeEvents([]);
    liveEventsRef.current = [];
    setViewingSessionId(undefined);
    setClaudeState((prev) => ({ ...prev, busy: false, queued: 0 }));
    vscode.postMessage({ type: 'claude:reset-session' });
  };

  const onPermissionMode = (mode: SupportedPermissionMode): void => {
    // Optimistic update so the pill flips instantly. Host's
    // claude:state will reconcile on the next publishState() tick.
    setClaudeState((prev) => ({ ...prev, mode }));
    vscode.postMessage({ type: 'claude:permission-mode', body: mode });
  };

  const onPermissionResponse = (
    requestId: string,
    behavior: 'allow' | 'deny' | 'always-allow',
  ): void => {
    vscode.postMessage({
      type: 'claude:permission-response',
      body: { requestId, behavior },
    });
  };

  return (
    <React.Fragment>
      <div className="room-meta">
        <span className="room-meta-topic">
          {topic ? `${topic.slice(0, 14)}…` : '(no room)'}
        </span>
        <span className="room-meta-nick">@{myNick}</span>
        <span className="room-meta-status">{status}</span>
        {topic && (
          <button
            type="button"
            className="room-meta-copy"
            onClick={() => vscode.postMessage({ type: 'room:copy-ticket' })}
            aria-label="Copy Ticket"
            title="Copy this Room's Ticket to clipboard"
          >
            <i className="codicon codicon-clippy" />
            <span>copy ticket</span>
          </button>
        )}
      </div>
      <div className="tab-strip" role="tablist">
        <button
          type="button"
          role="tab"
          className={`tab ${activeTab === 'chat' ? 'active' : ''}`}
          aria-selected={activeTab === 'chat'}
          onClick={() => setActiveTab('chat')}
        >
          <i className="codicon codicon-comment-discussion" />
          <span>Chat</span>
          {chatUnread > 0 && (
            <span className="tab-badge">{chatUnread > 99 ? '99+' : chatUnread}</span>
          )}
        </button>
        <button
          type="button"
          role="tab"
          className={`tab ${activeTab === 'claude' ? 'active' : ''}`}
          aria-selected={activeTab === 'claude'}
          onClick={() => setActiveTab('claude')}
        >
          <i className="codicon codicon-sparkle" />
          <span>Claude</span>
          {claudeState.busy && <i className="codicon codicon-loading codicon-modifier-spin tab-busy" />}
          {claudeUnread > 0 && !claudeState.busy && (
            <span className="tab-badge">{claudeUnread > 99 ? '99+' : claudeUnread}</span>
          )}
        </button>
      </div>
      <div className="panes">
        <div
          className={`pane-wrap ${activeTab === 'chat' ? 'active' : 'hidden'}`}
        >
          <Chat
            messages={messages}
            myNick={myNick}
            onSend={onSend}
            onAttach={onAttach}
            onPasteFiles={onPasteFiles}
            onOpenDrop={onOpenDrop}
            onSaveDrop={onSaveDrop}
          />
        </div>
        <div
          className={`pane-wrap ${activeTab === 'claude' ? 'active' : 'hidden'}`}
        >
          <Claude
            events={claudeEvents}
            state={claudeState}
            onPrompt={onPrompt}
            onInterrupt={onInterrupt}
            onResetSession={onResetSession}
            onOpenFile={onOpenFile}
            onPermissionMode={onPermissionMode}
            onPermissionResponse={onPermissionResponse}
            activeEditor={activeEditor}
            history={{
              viewing: viewingSessionId,
              sessions: historySessions,
              onRequestList: onRequestHistoryList,
              onLoad: onLoadHistory,
              onExit: onExitHistory,
            }}
          />
        </div>
      </div>
    </React.Fragment>
  );
}

const container = document.getElementById('root');
if (!container) throw new Error('webview root element missing');
createRoot(container).render(<App />);
