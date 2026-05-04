import * as React from 'react';
import { createRoot } from 'react-dom/client';
import { Chat } from './Chat';
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

  React.useEffect(() => {
    const onMsg = (event: MessageEvent): void => {
      const msg = (event.data ?? {}) as { type?: string; body?: unknown };
      if (msg.type === 'host:ready') {
        setStatus('host ready ✓ — tailing log.jsonl');
      } else if (msg.type === 'echo:reply') {
        setStatus(`host replied: ${String(msg.body)}`);
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
      }
    };
    window.addEventListener('message', onMsg);
    return () => window.removeEventListener('message', onMsg);
  }, []);

  const onEcho = (): void => {
    vscode.postMessage({
      type: 'echo:request',
      body: `ping at ${new Date().toISOString()}`,
    });
  };

  return (
    <React.Fragment>
      <h1>cc-connect — Room</h1>
      <p className="room-meta">
        topic: {topic ? `${topic.slice(0, 16)}…` : '(unknown)'} · me: {myNick}
      </p>
      <div className="panes">
        <Chat messages={messages} myNick={myNick} />
        <div className="pane">
          <h2>claude</h2>
          <div className="muted">(no Claude session — Step 4 will wire SDK)</div>
        </div>
      </div>
      <p className="actions">
        <button onClick={onEcho}>Echo to host</button>
      </p>
      <p className="status">{status}</p>
    </React.Fragment>
  );
}

const container = document.getElementById('root');
if (!container) throw new Error('webview root element missing');
createRoot(container).render(<App />);
