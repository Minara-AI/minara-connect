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
import type { Message } from "./types.ts";

export interface LogTailHandle {
  close(): void;
}

export type LogLineHandler = (msg: Message) => void;

export function logPathFor(topic: string): string {
  return join(homedir(), ".cc-connect", "rooms", topic, "log.jsonl");
}

/** Start tailing `log.jsonl`. The handler fires once per parsed message
 *  in chronological (file-order) sequence. Initial backlog (everything
 *  already in the file) is delivered first, then live appends.
 *
 *  Returns a handle whose `close()` cancels the watcher. */
export function tailLog(topic: string, onLine: LogLineHandler): LogTailHandle {
  const path = logPathFor(topic);
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
      // File truncated or replaced. Reset and re-read from 0.
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
      // Use fs.read with explicit position — works regardless of fd state.
      // (Node's fs.read positional form lets us read without seeking.)
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
              const msg = JSON.parse(line) as Message;
              onLine(msg);
            } catch {
              // Skip malformed lines — chat_session always writes valid
              // JSON, so this only happens if the log was corrupted by
              // something else. Log to stderr and keep going.
              // eslint-disable-next-line no-console
              console.error("log_tail: skipping malformed line:", line.slice(0, 80));
            }
          }
          nl = buf.indexOf("\n");
        }
      });
    });
  }

  // Kick off initial read.
  readMore();

  // Then watch. fs.watch on macOS uses kqueue + FSEvents and fires on
  // append; on Linux it's inotify. Both are fine for our append-only
  // workload. We re-stat + re-read on every event.
  try {
    watcher = watch(path, { persistent: true }, () => {
      readMore();
    });
  } catch {
    // File might not exist yet (daemon hasn't started). Poll periodically
    // until it does, then attach.
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
