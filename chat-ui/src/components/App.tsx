import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Box, useApp, useInput, useStdout } from "ink";
import { spawnSync } from "node:child_process";
import { HeaderBar } from "./HeaderBar.tsx";
import { ChatScrollback } from "./ChatScrollback.tsx";
import { InputBox } from "./InputBox.tsx";
import { MentionPopup } from "./MentionPopup.tsx";
import type { EventLine, Message } from "../types.ts";
import { tailEvents, tailLog } from "../log_tail.ts";
import { ccSend } from "../ipc.ts";
import { readChatDaemonPidFile, readSelfNick } from "../ticket.ts";
import { completeAt, currentAtToken, mentionCandidates } from "../mention.ts";

export interface AppProps {
  topic: string;
}

const RECENT_NICKS_CAP = 32;
const SCROLLBACK_CAP = 1024;
const SCROLL_STEP = 10;

/** Top-level state coordinator. Owns:
 *   - the messages list (fed by log_tail)
 *   - input buffer + scroll offset + mention popup state
 *   - global key dispatch (Ctrl-Q, Ctrl-Y, PgUp/PgDn, Tab, Enter, etc.) */
export function App({ topic }: AppProps) {
  const { exit } = useApp();
  const { stdout } = useStdout();
  const [messages, setMessages] = useState<Message[]>([]);
  const [events, setEvents] = useState<EventLine[]>([]);
  const [input, setInput] = useState("");
  const [scrollOffset, setScrollOffset] = useState(0);
  const [mentionIdx, setMentionIdx] = useState(0);
  const [mentionDismissed, setMentionDismissed] = useState(false);
  const [cursorBlink, setCursorBlink] = useState(true);
  const [statusFlash, setStatusFlash] = useState<string | null>(null);
  const recentNicksRef = useRef<string[]>([]);

  // Static facts read once at boot.
  const selfNick = useMemo(() => readSelfNick(), []);
  const pidFile = useMemo(() => readChatDaemonPidFile(topic), [topic]);
  const ticket = pidFile?.ticket ?? null;
  const selfPubkey: string | null = null; // we display by nick; pubkey not needed for own-detection

  // Tail log.jsonl into `messages`, capped at SCROLLBACK_CAP.
  useEffect(() => {
    const handle = tailLog(topic, (msg: Message) => {
      setMessages((prev) => {
        const next = prev.length >= SCROLLBACK_CAP ? prev.slice(1) : prev.slice();
        next.push(msg);
        return next;
      });
      // Track distinct nicks for the @-mention popup.
      const nick = msg.nick && msg.nick.length > 0 ? msg.nick : msg.author.slice(0, 8);
      const list = recentNicksRef.current;
      const idx = list.indexOf(nick);
      if (idx >= 0) list.splice(idx, 1);
      list.unshift(nick);
      while (list.length > RECENT_NICKS_CAP) list.pop();
    });
    return () => handle.close();
  }, [topic]);

  // Tail events.jsonl for daemon warnings (rate-limit, broadcast failure,
  // etc.). The daemon writes each Warn variant as `{ts, kind, body}` so
  // we can render them interleaved with chat lines by timestamp.
  useEffect(() => {
    const handle = tailEvents(topic, (ev: EventLine) => {
      setEvents((prev) => {
        const next = prev.length >= SCROLLBACK_CAP ? prev.slice(1) : prev.slice();
        next.push(ev);
        return next;
      });
    });
    return () => handle.close();
  }, [topic]);

  // Cursor blink.
  useEffect(() => {
    const t = setInterval(() => setCursorBlink((v) => !v), 500);
    return () => clearInterval(t);
  }, []);

  // Status flash auto-clear.
  useEffect(() => {
    if (statusFlash === null) return;
    const t = setTimeout(() => setStatusFlash(null), 2000);
    return () => clearTimeout(t);
  }, [statusFlash]);

  // Compute popup visibility + candidates each render — cheap, avoids
  // tracking open/close transitions explicitly.
  const popupToken = currentAtToken(input);
  const popupCandidates =
    popupToken !== null && !mentionDismissed
      ? mentionCandidates(recentNicksRef.current, popupToken, selfNick)
      : [];
  const popupVisible = popupCandidates.length > 0 && !mentionDismissed && popupToken !== null;

  const submit = useCallback(async () => {
    if (input.length === 0) return;
    const body = input;
    setInput("");
    setMentionIdx(0);
    setMentionDismissed(false);
    const resp = await ccSend(topic, body);
    if (!resp.ok) {
      setStatusFlash(`✗ send failed: ${resp.err ?? "unknown"}`);
    }
  }, [input, topic]);

  const copyTicket = useCallback(() => {
    if (ticket === null) {
      setStatusFlash("✗ no ticket (chat-daemon not started?)");
      return;
    }
    // Try pbcopy on macOS, xclip / xsel on Linux. Fall back to printing.
    const cmd = process.platform === "darwin" ? "pbcopy" : "xclip";
    const args = process.platform === "darwin" ? [] : ["-selection", "clipboard"];
    const r = spawnSync(cmd, args, { input: ticket });
    if (r.status === 0) {
      setStatusFlash("✓ ticket copied to clipboard");
    } else {
      setStatusFlash(`! ${cmd} not available — ticket: ${ticket.slice(0, 24)}…`);
    }
  }, [ticket]);

  useInput((char, key) => {
    // Global hotkeys take precedence regardless of focus.
    if (key.ctrl && (char === "q" || char === "Q" || char === "")) {
      exit();
      return;
    }
    if (key.ctrl && (char === "y" || char === "Y" || char === "")) {
      copyTicket();
      return;
    }
    if (key.pageUp) {
      setScrollOffset((s) => s + SCROLL_STEP);
      return;
    }
    if (key.pageDown) {
      setScrollOffset((s) => Math.max(0, s - SCROLL_STEP));
      return;
    }

    if (popupVisible) {
      if (key.upArrow) {
        setMentionIdx((i) => (i === 0 ? popupCandidates.length - 1 : i - 1));
        return;
      }
      if (key.downArrow) {
        setMentionIdx((i) => (i + 1) % popupCandidates.length);
        return;
      }
      if (key.tab || key.return) {
        const pick = popupCandidates[mentionIdx];
        if (pick) {
          setInput((cur) => completeAt(cur, pick));
          setMentionIdx(0);
        }
        return;
      }
      if (key.escape) {
        setMentionDismissed(true);
        setMentionIdx(0);
        return;
      }
      // fall through for character editing
    }

    if (key.return) {
      void submit();
      return;
    }
    if (key.backspace || key.delete) {
      setInput((s) => s.slice(0, -1));
      setMentionIdx(0);
      setMentionDismissed(false);
      return;
    }
    if (char && !key.ctrl && !key.meta) {
      setInput((s) => s + char);
      setMentionIdx(0);
      setMentionDismissed(false);
    }
  });

  // Visible row budget: total rows minus header (1) - input box (3 with border)
  // - popup (variable) - status (1 if flashing) - safety margin (1).
  const totalRows = stdout?.rows ?? 30;
  const popupRows = popupVisible ? popupCandidates.length + 3 : 0;
  const visibleRows = Math.max(3, totalRows - 1 - 3 - popupRows - (statusFlash ? 1 : 0) - 1);

  return (
    <Box flexDirection="column" height={totalRows}>
      <HeaderBar
        topicShort={topic.slice(0, 12)}
        selfNick={selfNick}
        daemonAlive={pidFile !== null}
      />
      <ChatScrollback
        messages={messages}
        events={events}
        selfPubkey={selfPubkey}
        selfNick={selfNick}
        scrollOffset={scrollOffset}
        visibleRows={visibleRows}
      />
      {popupVisible ? <MentionPopup candidates={popupCandidates} selectedIdx={mentionIdx} /> : null}
      {ticket && messages.length === 0 ? (
        <Box paddingX={1}>
          {/* @ts-ignore -- Text not imported here intentionally; use HeaderBar style */}
        </Box>
      ) : null}
      <InputBox value={input} cursorVisible={cursorBlink} />
      {statusFlash ? (
        <Box paddingX={1}>
          {/* status line */}
          <StatusLine text={statusFlash} />
        </Box>
      ) : null}
    </Box>
  );
}

import { Text } from "ink";

function StatusLine({ text }: { text: string }) {
  const isErr = text.startsWith("✗") || text.startsWith("!");
  return <Text color={isErr ? "red" : "green"}>{text}</Text>;
}
