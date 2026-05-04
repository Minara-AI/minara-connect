// Append-only tail for ~/.cc-connect/rooms/<topic>/log.jsonl, lifted
// from chat-ui/src/log_tail.ts. The chat-daemon writes log.jsonl
// fcntl-OFD-locked; we read with O_RDONLY which doesn't conflict
// (POSIX advisory locks don't block readers). Strategy:
//
//   1. Open file, read whole thing into a byte offset, parse each line.
//   2. fs.watch() the file. On any change, re-open + read from saved
//      offset to EOF, parse new lines, advance offset.
//
// Atomic-rename writes are handled by re-opening on each read.
// Truncation (file shrinks) resets offset to 0 — better to re-show
// than miss messages.

import {
  open,
  read as fsRead,
  close as fsClose,
  statSync,
  watch,
  type FSWatcher,
} from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import type { EventLine, Message } from '../types';

export interface LogTailHandle {
  close(): void;
}

export type LogLineHandler = (msg: Message) => void;
export type EventLineHandler = (ev: EventLine) => void;

export function logPathFor(topic: string): string {
  return join(homedir(), '.cc-connect', 'rooms', topic, 'log.jsonl');
}

export function eventsPathFor(topic: string): string {
  return join(homedir(), '.cc-connect', 'rooms', topic, 'events.jsonl');
}

/** Tail `log.jsonl`. Backlog (everything already in the file) is
 *  delivered first in chronological file order, then live appends. */
export function tailLog(topic: string, onLine: LogLineHandler): LogTailHandle {
  return tailJsonl<Message>(logPathFor(topic), onLine, 'log_tail');
}

/** Same append-only tail strategy, reading `events.jsonl` (the
 *  chat-daemon's ephemeral-notices side-channel — rate-limit warnings,
 *  system markers, etc.). */
export function tailEvents(
  topic: string,
  onEvent: EventLineHandler,
): LogTailHandle {
  return tailJsonl<EventLine>(eventsPathFor(topic), onEvent, 'events_tail');
}

function tailJsonl<T>(
  path: string,
  onLine: (parsed: T) => void,
  label: string,
): LogTailHandle {
  let offset = 0;
  let pending = false;
  let closed = false;
  let buf = '';
  let watcher: FSWatcher | null = null;

  function readMore(): void {
    if (closed || pending) return;
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
      buf = '';
    }
    if (size === offset) {
      pending = false;
      return;
    }

    open(path, 'r', (err, fd) => {
      if (err || closed) {
        pending = false;
        return;
      }
      const len = size - offset;
      const chunk = Buffer.allocUnsafe(len);
      fsRead(fd, chunk, 0, len, offset, (rerr, bytesRead) => {
        fsClose(fd, () => {});
        pending = false;
        if (rerr || closed) return;
        offset += bytesRead;
        buf += chunk.subarray(0, bytesRead).toString('utf8');
        let nl = buf.indexOf('\n');
        while (nl >= 0) {
          const line = buf.slice(0, nl).trim();
          buf = buf.slice(nl + 1);
          if (line.length > 0) {
            try {
              const parsed = JSON.parse(line) as T;
              onLine(parsed);
            } catch {
              // eslint-disable-next-line no-console
              console.error(
                `${label}: skipping malformed line:`,
                line.slice(0, 80),
              );
            }
          }
          nl = buf.indexOf('\n');
        }
      });
    });
  }

  readMore();

  try {
    watcher = watch(path, { persistent: true }, () => readMore());
  } catch {
    // File doesn't exist yet — poll until it does, then switch to watch.
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
    close(): void {
      closed = true;
      if (watcher) {
        try {
          watcher.close();
        } catch {
          /* swallow */
        }
      }
    },
  };
}
