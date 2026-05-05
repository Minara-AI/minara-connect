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
        setStatus('host ready ✓ — tailing log.jsonl');
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
        setClaudeEvents((prev) => [...prev, msg.body]);
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

  return (
    <React.Fragment>
      <p className="room-meta">
        topic: {topic ? `${topic.slice(0, 16)}…` : '(unknown)'} · me: {myNick} · {status}
      </p>
      <div className="panes">
        <Chat messages={messages} myNick={myNick} onSend={onSend} />
        <Claude events={claudeEvents} state={claudeState} />
      </div>
    </React.Fragment>
  );
}

const container = document.getElementById('root');
if (!container) throw new Error('webview root element missing');
createRoot(container).render(<App />);
