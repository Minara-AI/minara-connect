import * as React from 'react';
import { highlightMentions } from './highlightMentions';
import {
  completeAt,
  currentAtToken,
  mentionCandidates,
} from './mentionAutocomplete';
import type { Message } from './types';
import { useAutosize } from './useAutosize';
import { useStickyScroll } from './useStickyScroll';

interface ChatProps {
  messages: Message[];
  myNick: string;
  onSend?: (body: string) => void;
}

export function Chat({
  messages,
  myNick,
  onSend,
}: ChatProps): React.ReactElement {
  const [draft, setDraft] = React.useState('');
  const [popupOpen, setPopupOpen] = React.useState(false);
  const [popupCandidates, setPopupCandidates] = React.useState<string[]>([]);
  const [popupIndex, setPopupIndex] = React.useState(0);

  const scrollRef = useStickyScroll(messages.length);
  const textareaRef = useAutosize(draft);

  // Auto-focus the chat input on mount.
  React.useEffect(() => {
    textareaRef.current?.focus();
  }, [textareaRef]);

  // Recent nicks for @-mention autocomplete: most-recent-first,
  // unique, excluding self and own AI mirror.
  const recentNicks = React.useMemo(
    () => deriveRecentNicks(messages, myNick, 50),
    [messages, myNick],
  );

  const updatePopup = (text: string, cursor: number): void => {
    const upToCursor = text.slice(0, cursor);
    const tok = currentAtToken(upToCursor);
    if (tok === null) {
      setPopupOpen(false);
      return;
    }
    const cands = mentionCandidates(recentNicks, tok, myNick);
    if (cands.length === 0) {
      setPopupOpen(false);
      return;
    }
    setPopupCandidates(cands);
    setPopupIndex(0);
    setPopupOpen(true);
  };

  const submit = (): void => {
    const trimmed = draft.trim();
    if (!trimmed || !onSend) return;
    onSend(trimmed);
    setDraft('');
    setPopupOpen(false);
  };

  const accept = (full: string): void => {
    const next = completeAt(draft, full);
    setDraft(next);
    setPopupOpen(false);
    // Restore cursor to end on the next paint.
    requestAnimationFrame(() => {
      const ta = textareaRef.current;
      if (ta) ta.setSelectionRange(next.length, next.length);
    });
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    if (popupOpen) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setPopupIndex((i) => (i + 1) % popupCandidates.length);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setPopupIndex(
          (i) => (i - 1 + popupCandidates.length) % popupCandidates.length,
        );
        return;
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        e.preventDefault();
        accept(popupCandidates[popupIndex]);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        setPopupOpen(false);
        return;
      }
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.metaKey && !e.ctrlKey) {
      e.preventDefault();
      submit();
    }
  };

  const onChange = (e: React.ChangeEvent<HTMLTextAreaElement>): void => {
    const next = e.target.value;
    setDraft(next);
    updatePopup(next, e.target.selectionStart);
  };

  const empty = draft.trim().length === 0;

  return (
    <div className="pane">
      <div className="pane-head">chat</div>
      <div className="chat-log" ref={scrollRef}>
        {messages.length === 0 ? (
          <div className="muted">(no messages yet)</div>
        ) : (
          messages.map((m) => (
            <ChatBubble key={m.id} message={m} myNick={myNick} />
          ))
        )}
      </div>
      {onSend && (
        <div className="pane-input">
          {popupOpen && (
            <MentionPopup
              candidates={popupCandidates}
              selected={popupIndex}
              onPick={(i) => accept(popupCandidates[i])}
            />
          )}
          <textarea
            ref={textareaRef}
            value={draft}
            onChange={onChange}
            onKeyDown={onKeyDown}
            placeholder="Message — Enter to send · Shift+Enter for newline · /drop <path> · @ to mention"
            rows={1}
          />
          <button
            type="button"
            className="send-btn"
            onClick={submit}
            disabled={empty}
            aria-label="Send"
            title="Send (Enter)"
          >
            <SendIcon />
          </button>
        </div>
      )}
    </div>
  );
}

function MentionPopup({
  candidates,
  selected,
  onPick,
}: {
  candidates: string[];
  selected: number;
  onPick: (i: number) => void;
}): React.ReactElement {
  return (
    <div className="mention-popup" role="listbox">
      {candidates.map((c, i) => (
        <div
          key={c}
          className={`mention-item ${i === selected ? 'selected' : ''}`}
          onMouseDown={(e) => {
            // mousedown not click, so the textarea doesn't blur first.
            e.preventDefault();
            onPick(i);
          }}
          role="option"
          aria-selected={i === selected}
        >
          @{c}
        </div>
      ))}
    </div>
  );
}

function ChatBubble({
  message,
  myNick,
}: {
  message: Message;
  myNick: string;
}): React.ReactElement {
  const isMe = message.nick === myNick;
  const time = new Date(message.ts).toISOString().slice(11, 16);
  const nick = message.nick ?? 'anon';
  const initial = nick.charAt(0).toUpperCase() || '?';
  const avatarColor = colorForNick(nick);
  return (
    <div className={`chat-bubble ${isMe ? 'me' : 'peer'}`}>
      <div
        className="chat-avatar"
        style={{ background: avatarColor }}
        title={nick}
      >
        {initial}
      </div>
      <div className="chat-content">
        <div className="chat-meta">
          {isMe ? `${time} · ${nick}` : `${nick} · ${time}`}
        </div>
        <div className="chat-text">
          {highlightMentions(message.body, myNick)}
        </div>
      </div>
    </div>
  );
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

function deriveRecentNicks(
  messages: readonly Message[],
  myNick: string,
  limit: number,
): string[] {
  const me = myNick.toLowerCase();
  const meAi = me ? `${me}-cc` : '';
  const seen = new Set<string>();
  const out: string[] = [];
  for (let i = messages.length - 1; i >= 0 && out.length < limit; i--) {
    const nick = messages[i].nick;
    if (!nick) continue;
    const lower = nick.toLowerCase();
    if (lower === me || lower === meAi) continue;
    if (seen.has(lower)) continue;
    seen.add(lower);
    out.push(nick);
  }
  return out;
}

function colorForNick(nick: string): string {
  const palette = [
    '#5fa8d3',
    '#6ec07b',
    '#d39f5f',
    '#c46f6f',
    '#a56fc4',
    '#5fc4b9',
    '#d3c45f',
    '#7e88c4',
  ];
  let h = 0;
  for (let i = 0; i < nick.length; i++) {
    h = (h * 31 + nick.charCodeAt(i)) | 0;
  }
  return palette[Math.abs(h) % palette.length];
}
