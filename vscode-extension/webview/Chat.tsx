import * as React from 'react';
import { highlightMentions } from './highlightMentions';
import { MarkdownContent } from './MarkdownContent';
import {
  completeAt,
  currentAtToken,
  mentionCandidates,
} from './mentionAutocomplete';
import { focusTextareaAt } from './textareaFocus';
import { KIND_FILE_DROP, type Message } from './types';
import { useAutosize } from './useAutosize';
import { useStickyScroll } from './useStickyScroll';

function formatBytes(n: number | null | undefined): string {
  if (typeof n !== 'number' || n < 0) return '';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** AI peers carry a `<nick>-cc` suffix (see src/host/mention.ts).
 *  Their messages are model output and benefit from markdown
 *  rendering; humans keep plain text + mention highlighting. */
function isAiNick(nick: string | null | undefined): boolean {
  return !!nick && nick.endsWith('-cc');
}

interface ChatProps {
  messages: Message[];
  myNick: string;
  onSend?: (body: string) => void;
  onAttach?: () => void;
  onPasteFiles?: (files: { name: string; dataB64: string }[]) => void;
  onOpenDrop?: (filename: string) => void;
  onSaveDrop?: (filename: string) => void;
}

interface SlashCommand {
  cmd: string;
  label: string;
  template: string;
}

const SLASH_COMMANDS: readonly SlashCommand[] = [
  { cmd: '/drop', label: 'Share a file with the Room', template: '/drop ' },
  { cmd: '/at', label: 'At-mention a peer', template: '/at ' },
];

export function Chat({
  messages,
  myNick,
  onSend,
  onAttach,
  onPasteFiles,
  onOpenDrop,
  onSaveDrop,
}: ChatProps): React.ReactElement {
  const [draft, setDraft] = React.useState('');

  const [mentionOpen, setMentionOpen] = React.useState(false);
  const [mentionCands, setMentionCands] = React.useState<string[]>([]);
  const [mentionIndex, setMentionIndex] = React.useState(0);

  const [slashOpen, setSlashOpen] = React.useState(false);
  const [slashCands, setSlashCands] = React.useState<SlashCommand[]>([]);
  const [slashIndex, setSlashIndex] = React.useState(0);

  const scrollRef = useStickyScroll(messages.length);
  const textareaRef = useAutosize(draft);

  React.useEffect(() => {
    textareaRef.current?.focus();
  }, [textareaRef]);

  const recentNicks = React.useMemo(
    () => deriveRecentNicks(messages, myNick, 50),
    [messages, myNick],
  );

  const updatePopups = (text: string, cursor: number): void => {
    const upToCursor = text.slice(0, cursor);
    if (text.startsWith('/') && !upToCursor.includes(' ')) {
      const prefix = upToCursor.toLowerCase();
      const cands = SLASH_COMMANDS.filter((c) => c.cmd.startsWith(prefix));
      if (cands.length > 0) {
        setSlashCands(cands);
        setSlashIndex(0);
        setSlashOpen(true);
        setMentionOpen(false);
        return;
      }
    }
    setSlashOpen(false);

    const tok = currentAtToken(upToCursor);
    if (tok === null) {
      setMentionOpen(false);
      return;
    }
    const cands = mentionCandidates(recentNicks, tok, myNick);
    if (cands.length === 0) {
      setMentionOpen(false);
      return;
    }
    setMentionCands(cands);
    setMentionIndex(0);
    setMentionOpen(true);
  };

  const submit = (): void => {
    const trimmed = draft.trim();
    if (!trimmed || !onSend) return;
    onSend(trimmed);
    setDraft('');
    setMentionOpen(false);
    setSlashOpen(false);
  };

  const acceptMention = (full: string): void => {
    const next = completeAt(draft, full);
    setDraft(next);
    setMentionOpen(false);
    focusTextareaAt(textareaRef, next.length);
  };

  const acceptSlash = (template: string): void => {
    setDraft(template);
    setSlashOpen(false);
    focusTextareaAt(textareaRef, template.length);
  };

  const openSlashPicker = (): void => {
    setDraft('/');
    setSlashCands(SLASH_COMMANDS.slice());
    setSlashIndex(0);
    setSlashOpen(true);
    setMentionOpen(false);
    focusTextareaAt(textareaRef, 1);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    // IME composition (Pinyin, kana, etc.) — Enter finalizes the
    // candidate, never the message.
    if (e.nativeEvent.isComposing) return;
    if (slashOpen) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setSlashIndex((i) => (i + 1) % slashCands.length);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setSlashIndex(
          (i) => (i - 1 + slashCands.length) % slashCands.length,
        );
        return;
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        e.preventDefault();
        acceptSlash(slashCands[slashIndex].template);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        setSlashOpen(false);
        return;
      }
    }
    if (mentionOpen) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setMentionIndex((i) => (i + 1) % mentionCands.length);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setMentionIndex(
          (i) => (i - 1 + mentionCands.length) % mentionCands.length,
        );
        return;
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        e.preventDefault();
        acceptMention(mentionCands[mentionIndex]);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        setMentionOpen(false);
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
    updatePopups(next, e.target.selectionStart);
  };

  // When the clipboard has File items (screenshot, dragged-from-Finder
  // file, etc.), drop them into the Room instead of letting the
  // textarea paste their filename / data-URL. The webview is a
  // sandbox: we ferry bytes to the extension host as base64 and let
  // it write a temp file + cc_drop it.
  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>): void => {
    if (!onPasteFiles) return;
    const dt = e.clipboardData;
    if (!dt || dt.files.length === 0) return;
    e.preventDefault();
    const files = Array.from(dt.files);
    void Promise.all(
      files.map(async (f) => ({
        name: f.name || 'pasted-file',
        dataB64: await fileToBase64(f),
      })),
    ).then(onPasteFiles);
  };

  const empty = draft.trim().length === 0;

  // Group consecutive messages from the same nick within ~3 minutes
  // — show a compact row (avatar + nick + ts) for the leading message
  // and a continuation row (just body) for the rest. Reduces visual
  // weight and matches Slack/Discord compact mode.
  const grouped = React.useMemo(
    () => groupMessages(messages, myNick),
    [messages, myNick],
  );

  return (
    <div className="pane">
      <div className="chat-log" ref={scrollRef}>
        {grouped.length === 0 ? (
          <div className="muted-empty">
            <i className="codicon codicon-comment" />
            <span>no messages yet</span>
          </div>
        ) : (
          grouped.map((row) =>
            row.continuation ? (
              <ChatContinuation
                key={row.message.id}
                row={row}
                myNick={myNick}
                onOpenDrop={onOpenDrop}
                onSaveDrop={onSaveDrop}
              />
            ) : (
              <ChatRow
                key={row.message.id}
                row={row}
                myNick={myNick}
                onOpenDrop={onOpenDrop}
                onSaveDrop={onSaveDrop}
              />
            ),
          )
        )}
      </div>
      {onSend && (
        <div className="pane-input">
          {mentionOpen && (
            <MentionPopup
              candidates={mentionCands}
              selected={mentionIndex}
              onPick={(i) => acceptMention(mentionCands[i])}
            />
          )}
          {slashOpen && (
            <SlashPopup
              candidates={slashCands}
              selected={slashIndex}
              onPick={(i) => acceptSlash(slashCands[i].template)}
            />
          )}
          {onAttach && (
            <button
              type="button"
              className="icon-btn"
              onClick={onAttach}
              aria-label="Attach file"
              title="Attach a file"
            >
              <i className="codicon codicon-add" />
            </button>
          )}
          <button
            type="button"
            className="icon-btn"
            onClick={openSlashPicker}
            aria-label="Slash commands"
            title="Slash commands"
          >
            <i className="codicon codicon-symbol-event" />
          </button>
          <textarea
            ref={textareaRef}
            value={draft}
            onChange={onChange}
            onKeyDown={onKeyDown}
            onPaste={onPaste}
            placeholder="Message · Enter sends · @ to mention · paste a file to drop"
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
            <i className="codicon codicon-send" />
          </button>
        </div>
      )}
    </div>
  );
}

interface ChatRowData {
  message: Message;
  isMe: boolean;
  continuation: boolean;
}

function groupMessages(
  messages: readonly Message[],
  myNick: string,
): ChatRowData[] {
  const out: ChatRowData[] = [];
  for (let i = 0; i < messages.length; i++) {
    const m = messages[i];
    const isMe = m.nick === myNick;
    const prev = messages[i - 1];
    const continuation =
      !!prev &&
      prev.nick === m.nick &&
      m.ts - prev.ts < 3 * 60 * 1000; /* 3 min */
    out.push({ message: m, isMe, continuation });
  }
  return out;
}

interface ChatRowExtras {
  onOpenDrop?: (filename: string) => void;
  onSaveDrop?: (filename: string) => void;
}

function ChatBody({
  m,
  myNick,
  onOpenDrop,
  onSaveDrop,
}: {
  m: Message;
  myNick: string;
} & ChatRowExtras): React.ReactElement {
  if (m.kind === KIND_FILE_DROP) {
    return (
      <FileDropAttachment
        filename={m.body}
        size={m.blob_size ?? null}
        onOpen={onOpenDrop}
        onSave={onSaveDrop}
      />
    );
  }
  return isAiNick(m.nick) ? (
    <MarkdownContent text={m.body} />
  ) : (
    <>{highlightMentions(m.body, myNick)}</>
  );
}

function ChatRow({
  row,
  myNick,
  onOpenDrop,
  onSaveDrop,
}: {
  row: ChatRowData;
  myNick: string;
} & ChatRowExtras): React.ReactElement {
  const m = row.message;
  // Local timezone, not UTC. `toISOString().slice(11,16)` showed UTC,
  // which doesn't match peers' wall clocks across zones.
  const time = new Date(m.ts).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
  });
  const nick = m.nick ?? 'anon';
  const initial = nick.charAt(0).toUpperCase() || '?';
  const avatarColor = colorForNick(nick);
  return (
    <div className={`chat-row ${row.isMe ? 'me' : 'peer'}`}>
      <div
        className="chat-avatar"
        style={{ background: avatarColor }}
        title={nick}
      >
        {initial}
      </div>
      <div className="chat-body">
        <div className="chat-byline">
          <span className="chat-nick">{nick}</span>
          <span className="chat-ts">{time}</span>
        </div>
        <div className="chat-text">
          <ChatBody
            m={m}
            myNick={myNick}
            onOpenDrop={onOpenDrop}
            onSaveDrop={onSaveDrop}
          />
        </div>
      </div>
    </div>
  );
}

function ChatContinuation({
  row,
  myNick,
  onOpenDrop,
  onSaveDrop,
}: {
  row: ChatRowData;
  myNick: string;
} & ChatRowExtras): React.ReactElement {
  return (
    <div className={`chat-row continuation ${row.isMe ? 'me' : 'peer'}`}>
      <div className="chat-avatar-spacer" />
      <div className="chat-body">
        <div className="chat-text">
          <ChatBody
            m={row.message}
            myNick={myNick}
            onOpenDrop={onOpenDrop}
            onSaveDrop={onSaveDrop}
          />
        </div>
      </div>
    </div>
  );
}

function FileDropAttachment({
  filename,
  size,
  onOpen,
  onSave,
}: {
  filename: string;
  size: number | null;
  onOpen?: (filename: string) => void;
  onSave?: (filename: string) => void;
}): React.ReactElement {
  const sizeLabel = formatBytes(size);
  return (
    <div className="file-drop">
      <button
        type="button"
        className="file-drop-main"
        onClick={() => onOpen?.(filename)}
        title={`Open ${filename}`}
        disabled={!onOpen}
      >
        <i className="codicon codicon-file" />
        <span className="file-drop-name">{filename}</span>
        {sizeLabel && <span className="file-drop-size">{sizeLabel}</span>}
      </button>
      {onSave && (
        <button
          type="button"
          className="file-drop-save"
          onClick={() => onSave(filename)}
          title={`Save ${filename} to a chosen location`}
          aria-label="Save copy"
        >
          <i className="codicon codicon-cloud-download" />
        </button>
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

function SlashPopup({
  candidates,
  selected,
  onPick,
}: {
  candidates: SlashCommand[];
  selected: number;
  onPick: (i: number) => void;
}): React.ReactElement {
  return (
    <div className="mention-popup slash-popup" role="listbox">
      {candidates.map((c, i) => (
        <div
          key={c.cmd}
          className={`mention-item ${i === selected ? 'selected' : ''}`}
          onMouseDown={(e) => {
            e.preventDefault();
            onPick(i);
          }}
          role="option"
          aria-selected={i === selected}
        >
          <span className="slash-cmd">{c.cmd}</span>
          <span className="slash-label">{c.label}</span>
        </div>
      ))}
    </div>
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

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onerror = () => reject(r.error ?? new Error('read failed'));
    r.onload = () => {
      const result = r.result;
      if (typeof result !== 'string') {
        reject(new Error('expected data URL'));
        return;
      }
      // result is `data:<mime>;base64,<payload>` — strip prefix.
      const comma = result.indexOf(',');
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    r.readAsDataURL(file);
  });
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
