// Mirrors crates/cc-connect-core/src/message.rs::Message and
// crates/cc-connect/src/chat_session.rs::MentionEvent. Types stay
// hand-written (not generated) — the Rust side is the source of truth
// and changes rarely; runtime validation happens at the parse boundary
// in `log_tail.ts` and `ipc.ts`.

export const KIND_CHAT = "chat";
export const KIND_FILE_DROP = "file_drop";

/** One on-the-wire chat message, as serialised in log.jsonl. */
export interface Message {
  /** ULID, monotonic. */
  id: string;
  /** Author Pubkey (base32). */
  author: string;
  /** Self-declared display name. May be `null`/missing on legacy lines. */
  nick?: string | null;
  /** Unix-ms wall clock at send time. */
  ts: number;
  /** "chat" or "file_drop". */
  kind: string;
  /** For chat: body text. For file_drop: filename. */
  body: string;
  /** file_drop only: blob hash (hex). */
  blob_hash?: string | null;
  /** file_drop only: bytes. */
  blob_size?: number | null;
}

/** Returned by the chat-daemon's `wait_for_mention` IPC action. */
export interface MentionEvent {
  id: string;
  ts: number;
  nick: string;
  body: string;
}

/** Chat-daemon PID file at ~/.cc-connect/rooms/<topic>/chat-daemon.pid. */
export interface ChatDaemonPidFile {
  pid: number;
  topic: string;
  ticket: string;
  started_at: number;
  relay: string | null;
  no_relay: boolean;
}
