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
        // Always append to the live buffer so "return to live"
        // restores everything Claude said while we were browsing
        // history.
        liveEventsRef.current = [...liveEventsRef.current, stamped];
        if (!viewingRef.current) {
          setClaudeEvents(liveEventsRef.current);
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
    // Drain any messages buffered between script-load and now, then
    // detach the early-handler so duplicates can't fire.
    if (earlyHandler) {
      window.removeEventListener('message', earlyHandler);
      earlyHandler = null;
    }
    for (const buffered of earlyMessages) onMsg(buffered);
    earlyMessages.length = 0;

    window.addEventListener('message', onMsg);
    return () => window.removeEventListener('message', onMsg);
  }, []);

  const onSend = (body: string): void => {
    vscode.postMessage({ type: 'chat:send', body });
  };

  const onAttach = (): void => {
    vscode.postMessage({ type: 'chat:attach' });
  };

  const onPrompt = (body: string): void => {
    vscode.postMessage({ type: 'claude:prompt', body });
    // Synthesize a local-only event so the prompt renders in the
    // Claude log immediately. SDK events stream back without echoing
    // the user side, and we want chips + scrollback parity.
    const synthetic = { type: 'cc:user-prompt', body, _receivedAt: Date.now() };
    liveEventsRef.current = [...liveEventsRef.current, synthetic];
    setClaudeEvents(liveEventsRef.current);
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
    setClaudeEvents(liveEventsRef.current);
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
