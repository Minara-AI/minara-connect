# Claude pane UI optimization plan

> **Status: shipped (vscode-extension-v0.2.1, 2026-05-05).**
> Historical record. The original 13-item TODO list lived here while
> we iterated against Anthropic's official VSCode extension as a
> reference for *visible behavior only* (no code lift). Most items
> shipped; the rest are deferred or out of scope. Git log is
> authoritative for "what changed when".

## Shipped

| ID | Item | Commit |
|---|---|---|
| T1.1 | Step-list timeline with vertical connector | `96f1113` (later refined to bullet-only in `02b4770`) |
| T1.2 | Stop button (interrupt current turn) | `ede84cc` |
| T1.3 | Permission-mode pill (auto / ask edits / plan / ask all) + inline approval bubbles for `default` | `46e3822` (pill) + `9f5a144` (inline bubble) |
| T1.4 | "Queue another message…" placeholder + queue-depth pill | `ea7441f` |
| T1.5 | "Thought for Xs" thinking indicator | `8c634b1` |
| T2.1 | Slash-command launcher (`/` button) | `600ddd8` |
| T2.2 | Attach button (`+`) → file drop dialog | `600ddd8` |
| T2.3 | Conversation history per workspace (`⏰` icon → HistoryPicker) | `46e3822` |
| T2.4 | "New chat" — fork a fresh Claude session | `4a7e9dc` |
| T2.5 v0 | Conversation title (first user prompt, system-wrappers stripped) | `46e3822` |
| T3.1 | File-reference chips in user prompts | `46e3822` |
| T3.2 | Tool call IN/OUT layout | `46e3822` |
| T3.3 | Step state colors (●/○/✗) | rolled into T1.1 |

## Deferred / out of scope

| ID | Item | Why deferred |
|---|---|---|
| T2.5 v1 | Claude-summarized conversation titles (vs first-prompt v0) | Quota-burning per session close; v0 is good enough for in-session navigation. Revisit if title quality becomes a friction point. |
| T3.4 | Voice input | Requires `MediaRecorder` + a transcription service; not worth the dependency footprint for a v0 niche. |

## Working agreement (still in force)

- **Don't lift code** from the official Claude Code VSCode extension.
  Patterns are observable; specific TS/React code is proprietary.
- **Don't read** `~/work/claude-code-main` or any reconstructed-from-
  source-map repos. cc-connect is MIT/Apache; mixing in leaked source
  taints contributors.
- **Reference [sugyan/claude-code-webui](https://github.com/sugyan/claude-code-webui)**
  (MIT, 1.1k★) when stuck on similar problems — that's the OSS
  analog. `webview/processClaude.ts`'s architecture is acknowledged
  as cribbed from sugyan's `UnifiedMessageProcessor` (with attribution
  in the file header).

## Where the live polish lives now

- Tool cards, IN/OUT split: [`webview/ToolCard.tsx`](../webview/ToolCard.tsx)
- Permission UI: [`webview/PermissionBubble.tsx`](../webview/PermissionBubble.tsx) + runner [`src/host/claude_runner.ts`](../src/host/claude_runner.ts) (canUseTool path)
- History picker: [`webview/HistoryPicker.tsx`](../webview/HistoryPicker.tsx) + transcripts reader [`src/host/transcripts.ts`](../src/host/transcripts.ts)
- Chat layout (iMessage right-aligned own messages): CSS in [`src/panel/RoomPanelProvider.ts`](../src/panel/RoomPanelProvider.ts) `.chat-row` rules
- File-ref chips: [`webview/fileRefs.ts`](../webview/fileRefs.ts)

For new UX ideas, file a GitHub issue with the `vscode-extension`
label rather than appending to this file.
