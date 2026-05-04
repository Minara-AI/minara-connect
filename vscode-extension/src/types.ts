// Mirrors crates/cc-connect-core/src/message.rs::Message — kept in sync
// by hand with chat-ui/src/types.ts and webview/types.ts. The shape is
// stable; if it ever needs to change, update all three in one PR.

export const KIND_CHAT = 'chat';
export const KIND_FILE_DROP = 'file_drop';

export interface Message {
  id: string;
  author: string;
  nick?: string | null;
  ts: number;
  kind: string;
  body: string;
  blob_hash?: string | null;
  blob_size?: number | null;
}

export interface EventLine {
  ts: number;
  kind: string;
  body: string;
}
