import React, { useMemo } from "react";
import { Box, Text } from "ink";
import type { Message } from "../types.ts";
import { KIND_FILE_DROP } from "../types.ts";
import { bodyMentionsSelf } from "../mention.ts";

export interface ChatScrollbackProps {
  messages: readonly Message[];
  /** Set of pubkeys we sent (so we can right-align our own bubbles). */
  selfPubkey: string | null;
  selfNick: string | null;
  /** Lines from the live bottom; 0 = follow tail. */
  scrollOffset: number;
  /** Visible row budget (terminal rows minus header / input / popup). */
  visibleRows: number;
}

interface RenderRow {
  key: string;
  ts: number;
  nick: string;
  body: string;
  isOwn: boolean;
  isMention: boolean;
  isFileDrop: boolean;
}

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
  selfPubkey,
  selfNick,
  scrollOffset,
  visibleRows,
}: ChatScrollbackProps) {
  const rows = useMemo<RenderRow[]>(
    () =>
      messages.map((m) => {
        const isOwn = selfPubkey !== null && m.author === selfPubkey;
        const isFileDrop = m.kind === KIND_FILE_DROP;
        return {
          key: m.id,
          ts: m.ts,
          nick: nickFor(m),
          body: isFileDrop ? `dropped ${m.body}` : m.body,
          isOwn,
          // Visual mention only on incoming lines — own messages don't
          // self-flag (the rendering rule, separate from the wake-up rule).
          isMention: !isOwn && bodyMentionsSelf(m.body, selfNick),
          isFileDrop,
        };
      }),
    [messages, selfPubkey, selfNick],
  );

  // Apply scroll: 0 = show last `visibleRows`. >0 holds N rows back.
  const total = rows.length;
  const end = Math.max(0, total - scrollOffset);
  const start = Math.max(0, end - visibleRows);
  const visible = rows.slice(start, end);

  return (
    <Box flexDirection="column" flexGrow={1}>
      {visible.map((r) => (
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
      ))}
      {scrollOffset > 0 ? (
        <Text dimColor>… {scrollOffset} rows back · PgDn to follow</Text>
      ) : null}
    </Box>
  );
}
