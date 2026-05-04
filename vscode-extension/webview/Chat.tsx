import * as React from 'react';
import type { Message } from './types';

interface ChatProps {
  messages: Message[];
  myNick: string;
}

export function Chat({ messages, myNick }: ChatProps): React.ReactElement {
  return (
    <div className="pane">
      <h2>chat</h2>
      <div className="chat-log">
        {messages.length === 0 ? (
          <div className="muted">(no messages yet — waiting for log.jsonl tail…)</div>
        ) : (
          messages.map((m) => <ChatLine key={m.id} message={m} myNick={myNick} />)
        )}
      </div>
    </div>
  );
}

function ChatLine({ message, myNick }: { message: Message; myNick: string }): React.ReactElement {
  const isMe = message.nick === myNick;
  const time = new Date(message.ts).toISOString().slice(11, 19);
  return (
    <div className={`chat-line ${isMe ? 'me' : 'peer'}`}>
      <span className="ts">{time}</span>
      <span className="nick">{message.nick ?? '(anon)'}</span>
      <span className="body">{message.body}</span>
    </div>
  );
}
