// Append-only tail for ~/.cc-connect/rooms/<topic>/log.jsonl.
//
// log.jsonl is fcntl-OFD-locked by chat_session on writes; we read with
// O_RDONLY which doesn't conflict (POSIX advisory locks don't block
// readers). Strategy:
//
//   1. Open file, read whole thing into a byte offset, parse each line.
//   2. fs.watch() the file. On any change, re-open + read from saved
//      offset to EOF, parse new lines, advance offset.
//
// Atomic-rename writes (rare in our flow but possible) are handled by
// re-opening on each read. Truncation (file shrinks) resets offset to 0
// — better to re-show than miss messages.

import { open, statSync, watch, type FSWatcher } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { EventLine, Message } from "./types.ts";

export interface LogTailHandle {
  close(): void;
}

export type LogLineHandler = (msg: Message) => void;

export function logPathFor(topic: string): string {
  return join(homedir(), ".cc-connect", "rooms", topic, "log.jsonl");
}

export function eventsPathFor(topic: string): string {
  return join(homedir(), ".cc-connect", "rooms", topic, "events.jsonl");
}

/** Start tailing `log.jsonl`. The handler fires once per parsed message
 *  in chronological (file-order) sequence. Initial backlog (everything
 *  already in the file) is delivered first, then live appends.
 *
 *  Returns a handle whose `close()` cancels the watcher. */
export function tailLog(topic: string, onLine: LogLineHandler): LogTailHandle {
  return tailJsonl<Message>(logPathFor(topic), onLine, "log_tail");
}

export type EventLineHandler = (ev: EventLine) => void;

/** Same append-only tail strategy, reading `events.jsonl` (the
 *  chat-daemon's ephemeral-notices side-channel). Each line is a JSON
 *  object with `{ts, kind, body}`. */
export function tailEvents(topic: string, onEvent: EventLineHandler): LogTailHandle {
  return tailJsonl<EventLine>(eventsPathFor(topic), onEvent, "events_tail");
}

/** Generic newline-JSON tail with byte-offset tracking + fs.watch
 *  fallback to polling. Both `log.jsonl` and `events.jsonl` are
 *  append-only; if the file shrinks (truncation), we reset offset to 0
 *  and re-read rather than miss data. */
function tailJsonl<T>(
  path: string,
  onLine: (parsed: T) => void,
  label: string,
): LogTailHandle {
  let offset = 0;
  let pending = false;
  let closed = false;
  let buf = "";
  let watcher: FSWatcher | null = null;

  function readMore() {
    if (closed) return;
    if (pending) return;
    pending = true;

    let size = 0;
    try {
      size = statSync(path).size;
    } catch {
      pending = false;
      return;
    }
    if (size < offset) {
      offset = 0;
      buf = "";
    }
    if (size === offset) {
      pending = false;
      return;
    }

    open(path, "r", (err, fd) => {
      if (err || closed) {
        pending = false;
        return;
      }
      const len = size - offset;
      const chunk = Buffer.allocUnsafe(len);
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      const { read, close: closeFd } = require("node:fs") as typeof import("node:fs");
      read(fd, chunk, 0, len, offset, (rerr, bytesRead) => {
        closeFd(fd, () => {});
        pending = false;
        if (rerr || closed) return;
        offset += bytesRead;
        buf += chunk.subarray(0, bytesRead).toString("utf8");
        let nl = buf.indexOf("\n");
        while (nl >= 0) {
          const line = buf.slice(0, nl).trim();
          buf = buf.slice(nl + 1);
          if (line.length > 0) {
            try {
              const parsed = JSON.parse(line) as T;
              onLine(parsed);
            } catch {
              // eslint-disable-next-line no-console
              console.error(`${label}: skipping malformed line:`, line.slice(0, 80));
            }
          }
          nl = buf.indexOf("\n");
        }
      });
    });
  }

  readMore();

  try {
    watcher = watch(path, { persistent: true }, () => {
      readMore();
    });
  } catch {
    const poll = setInterval(() => {
      if (closed) {
        clearInterval(poll);
        return;
      }
      try {
        statSync(path);
        clearInterval(poll);
        watcher = watch(path, { persistent: true }, () => readMore());
        readMore();
      } catch {
        // not yet
      }
    }, 500);
  }

  return {
    close() {
      closed = true;
      if (watcher) {
        try {
          watcher.close();
        } catch {}
      }
    },
  };
}
