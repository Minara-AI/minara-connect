import React, { useMemo } from "react";
import { Box, Text } from "ink";
import type { EventLine, Message } from "../types.ts";
import { KIND_FILE_DROP } from "../types.ts";
import { bodyMentionsSelf } from "../mention.ts";

export interface ChatScrollbackProps {
  messages: readonly Message[];
  /** Daemon-emitted ephemeral notices (rate-limit warnings, etc.). Read
   *  from events.jsonl. Rendered interleaved with messages by timestamp,
   *  styled distinctly so they don't get confused with chat. */
  events: readonly EventLine[];
  /** Set of pubkeys we sent (so we can right-align our own bubbles). */
  selfPubkey: string | null;
  selfNick: string | null;
  /** Lines from the live bottom; 0 = follow tail. */
  scrollOffset: number;
  /** Visible row budget (terminal rows minus header / input / popup). */
  visibleRows: number;
}

type RenderRow =
  | {
      kind: "msg";
      key: string;
      ts: number;
      nick: string;
      body: string;
      isOwn: boolean;
      isMention: boolean;
      isFileDrop: boolean;
    }
  | {
      kind: "event";
      key: string;
      ts: number;
      eventKind: string;
      body: string;
    };

function formatHHMM(tsMs: number): string {
  const totalMin = Math.floor(tsMs / 60000);
  const dayMin = ((totalMin % 1440) + 1440) % 1440;
  const hh = Math.floor(dayMin / 60).toString().padStart(2, "0");
  const mm = (dayMin % 60).toString().padStart(2, "0");
  return `${hh}:${mm}`;
}

function nickFor(msg: Message): string {
  if (msg.nick && msg.nick.length > 0) return msg.nick;
  return msg.author.slice(0, 8);
}

/** L/R bubble layout. Own messages right-aligned, peer messages left.
 *  Mentions of self get a red `(@me)` prefix on peer lines. */
export function ChatScrollback({
  messages,
  events,
  selfPubkey,
  selfNick,
  scrollOffset,
  visibleRows,
}: ChatScrollbackProps) {
  const rows = useMemo<RenderRow[]>(() => {
    const msgRows: RenderRow[] = messages.map((m) => {
      const isOwn = selfPubkey !== null && m.author === selfPubkey;
      const isFileDrop = m.kind === KIND_FILE_DROP;
      return {
        kind: "msg",
        key: m.id,
        ts: m.ts,
        nick: nickFor(m),
        body: isFileDrop ? `dropped ${m.body}` : m.body,
        isOwn,
        isMention: !isOwn && bodyMentionsSelf(m.body, selfNick),
        isFileDrop,
      };
    });
    const eventRows: RenderRow[] = events.map((e, i) => ({
      kind: "event",
      // events.jsonl has no id; ts+i is unique-enough within a session.
      key: `ev-${e.ts}-${i}`,
      ts: e.ts,
      eventKind: e.kind,
      body: e.body,
    }));
    // Stable timestamp-merge. JS sort is stable for equal keys, so events
    // and messages with identical ts keep their relative file order
    // within their kind.
    return [...msgRows, ...eventRows].sort((a, b) => a.ts - b.ts);
  }, [messages, events, selfPubkey, selfNick]);

  const total = rows.length;
  const end = Math.max(0, total - scrollOffset);
  const start = Math.max(0, end - visibleRows);
  const visible = rows.slice(start, end);

  return (
    <Box flexDirection="column" flexGrow={1}>
      {visible.map((r) =>
        r.kind === "msg" ? (
          <Box
            key={r.key}
            flexDirection="row"
            justifyContent={r.isOwn ? "flex-end" : "flex-start"}
          >
            <Text>
              <Text dimColor>{formatHHMM(r.ts)} </Text>
              {r.isMention ? <Text color="red" bold>(@me) </Text> : null}
              <Text bold color={r.isOwn ? "green" : "cyan"}>
                [{r.nick}]
              </Text>{" "}
              <Text color={r.isMention ? "red" : undefined}>{r.body}</Text>
            </Text>
          </Box>
        ) : (
          <Box key={r.key} flexDirection="row">
            <Text color="yellow" dimColor>
              {formatHHMM(r.ts)} ⚠ {r.body}
            </Text>
          </Box>
        ),
      )}
      {scrollOffset > 0 ? (
        <Text dimColor>… {scrollOffset} rows back · PgDn to follow</Text>
      ) : null}
    </Box>
  );
}
