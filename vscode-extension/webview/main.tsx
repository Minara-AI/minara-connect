import * as React from 'react';
import { createRoot } from 'react-dom/client';
import { Chat } from './Chat';
import { Claude, type ClaudeRunnerState } from './Claude';
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

function App(): React.ReactElement {
  const [status, setStatus] = React.useState('waiting for host…');
  const [messages, setMessages] = React.useState<Message[]>([]);
  const [myNick, setMyNick] = React.useState('(me)');
  const [topic, setTopic] = React.useState('');
  const [claudeEvents, setClaudeEvents] = React.useState<unknown[]>([]);
  const [claudeState, setClaudeState] = React.useState<ClaudeRunnerState>({
    busy: false,
    queued: 0,
  });

  React.useEffect(() => {
    const onMsg = (event: MessageEvent): void => {
      const msg = (event.data ?? {}) as { type?: string; body?: unknown };
      if (msg.type === 'host:ready') {
        setStatus('ready');
      } else if (msg.type === 'room:reset') {
        // Switching rooms — wipe all per-Room webview state so the
        // new Room's backfill streams in cleanly.
        setMessages([]);
        setClaudeEvents([]);
        setClaudeState({ busy: false, queued: 0 });
        setTopic('');
        setStatus('switching…');
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
      } else if (msg.type === 'chat:send-error') {
        setStatus(`send failed: ${String(msg.body)}`);
      } else if (msg.type === 'claude:event') {
        // Stamp with arrival time so processClaude can compute the
        // "Thought for Xs" gap between session start and first content.
        const stamped =
          msg.body && typeof msg.body === 'object'
            ? { ...(msg.body as object), _receivedAt: Date.now() }
            : msg.body;
        setClaudeEvents((prev) => [...prev, stamped]);
      } else if (msg.type === 'claude:state') {
        const s = msg.body as ClaudeRunnerState;
        setClaudeState({ busy: !!s.busy, queued: s.queued ?? 0 });
      }
    };
    window.addEventListener('message', onMsg);
    return () => window.removeEventListener('message', onMsg);
  }, []);

  const onSend = (body: string): void => {
    vscode.postMessage({ type: 'chat:send', body });
  };

  const onPrompt = (body: string): void => {
    vscode.postMessage({ type: 'claude:prompt', body });
  };

  const onInterrupt = (): void => {
    vscode.postMessage({ type: 'claude:interrupt' });
  };

  return (
    <React.Fragment>
      <div className="room-meta">
        {topic ? `${topic.slice(0, 16)}…` : '(no room)'} · me: {myNick} · {status}
      </div>
      <div className="panes">
        <Chat messages={messages} myNick={myNick} onSend={onSend} />
        <Claude
          events={claudeEvents}
          state={claudeState}
          onPrompt={onPrompt}
          onInterrupt={onInterrupt}
        />
      </div>
    </React.Fragment>
  );
}

const container = document.getElementById('root');
if (!container) throw new Error('webview root element missing');
createRoot(container).render(<App />);
