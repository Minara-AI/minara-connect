// Bottom-panel webview view provider. Owns one Room view at a time;
// switching Rooms tears down the active tail / Claude runner and
// re-resolves the webview HTML so React state starts fresh.
//
// Single-instance by VSCode contract: the panel area shows the view
// once. The user docks / undocks / resizes via the standard panel
// chrome, which gives us the "top editor + bottom Room 50/50" layout
// for free (the user drags the divider).

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as vscode from 'vscode';
import {
  createClaudeRunner,
  type ClaudeRunnerHandle,
} from '../host/claude_runner';
import { ccDrop, ccSend } from '../host/ipc';
import { tailLog, type LogTailHandle } from '../host/log_tail';
import { shouldWakeClaude } from '../host/mention';
import type { Message } from '../types';

export class RoomPanelProvider implements vscode.WebviewViewProvider {
  static readonly viewType = 'cc-connect.room';

  private view?: vscode.WebviewView;
  private currentTopic?: string;
  private myNick = '(me)';
  private tail?: LogTailHandle;
  private runner?: ClaudeRunnerHandle;
  private messageDisposable?: vscode.Disposable;

  constructor(private readonly context: vscode.ExtensionContext) {}

  /** Set (or replace) the Room shown in the panel. If the view isn't
   *  resolved yet (panel never shown), stash the topic and apply on
   *  the next resolveWebviewView call. */
  setRoom(topic: string): void {
    this.currentTopic = topic;
    if (this.view) this.activateRoom(topic);
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    const distRoot = vscode.Uri.joinPath(
      this.context.extensionUri,
      'dist',
      'webview',
    );
    view.webview.options = {
      enableScripts: true,
      localResourceRoots: [distRoot],
    };

    view.onDidDispose(() => {
      this.tearDown();
      this.view = undefined;
    });

    if (this.currentTopic) {
      this.activateRoom(this.currentTopic);
    } else {
      view.webview.html = idleHtml(view.webview);
    }
  }

  private tearDown(): void {
    this.messageDisposable?.dispose();
    this.messageDisposable = undefined;
    this.tail?.close();
    this.tail = undefined;
    this.runner?.abort();
    this.runner = undefined;
  }

  private activateRoom(topic: string): void {
    const view = this.view;
    if (!view) return;
    this.tearDown();

    const distRoot = vscode.Uri.joinPath(
      this.context.extensionUri,
      'dist',
      'webview',
    );
    view.webview.html = roomHtml(view.webview, distRoot);
    this.myNick = readMyNick() ?? '(me)';

    this.messageDisposable = view.webview.onDidReceiveMessage(
      async (msg: { type?: string; body?: unknown }) => {
        const t = this.currentTopic;
        if (!t) return;
        if (msg.type === 'chat:send') {
          const body = typeof msg.body === 'string' ? msg.body.trim() : '';
          if (!body) return;
          const dropMatch = /^\/drop\s+(.+)$/.exec(body);
          const resp = dropMatch
            ? await ccDrop(t, dropMatch[1].trim())
            : await ccSend(t, body);
          if (!resp.ok) {
            view.webview.postMessage({
              type: 'chat:send-error',
              body: resp.err ?? 'unknown ipc error',
            });
          }
        } else if (msg.type === 'claude:prompt') {
          // Direct prompt to the local Claude — bypasses the chat
          // substrate entirely, doesn't broadcast to peers.
          const body = typeof msg.body === 'string' ? msg.body.trim() : '';
          if (!body) return;
          this.runner?.enqueue(body);
        } else if (msg.type === 'claude:interrupt') {
          // Cancel the in-flight turn only; queued prompts still run.
          this.runner?.interrupt();
        } else if (msg.type === 'claude:reset-session') {
          // Mint a fresh sessionId; the webview clears its local
          // state in parallel via `room:claude-cleared`.
          this.runner?.resetSession();
        } else if (msg.type === 'chat:attach') {
          // Open VSCode's native file picker, then drop whatever the
          // user selects into the Room. Cancellation = silent no-op.
          const picked = await vscode.window.showOpenDialog({
            canSelectFiles: true,
            canSelectMany: false,
            openLabel: 'Drop into Room',
          });
          if (!picked || picked.length === 0) return;
          const filePath = picked[0].fsPath;
          const resp = await ccDrop(t, filePath);
          if (!resp.ok) {
            view.webview.postMessage({
              type: 'chat:send-error',
              body: resp.err ?? 'unknown ipc error',
            });
          }
        }
      },
    );

    // Tell webview to clear React state from any prior Room before
    // streaming the new one's backfill in.
    view.webview.postMessage({ type: 'room:reset' });
    view.webview.postMessage({
      type: 'room:state',
      body: { topic, myNick: this.myNick },
    });

    this.runner = createClaudeRunner({
      topic,
      onEvent: (event) =>
        view.webview.postMessage({ type: 'claude:event', body: event }),
      onStateChange: (state) =>
        view.webview.postMessage({ type: 'claude:state', body: state }),
    });

    try {
      this.tail = tailLog(topic, (m: Message) => {
        view.webview.postMessage({ type: 'chat:message', body: m });
        const fromOwnAi = !!this.myNick && m.nick === `${this.myNick}-cc`;
        if (
          !fromOwnAi &&
          this.myNick &&
          shouldWakeClaude(m.body, this.myNick)
        ) {
          this.runner?.enqueue(m.body);
        }
      });
    } catch (e) {
      const err = e instanceof Error ? e.message : String(e);
      void vscode.window.showErrorMessage(
        `cc-connect: log tail failed — ${err}`,
      );
    }

    view.webview.postMessage({ type: 'host:ready' });
  }
}

function readMyNick(): string | undefined {
  try {
    const p = path.join(os.homedir(), '.cc-connect', 'config.json');
    const cfg = JSON.parse(fs.readFileSync(p, 'utf8')) as {
      self_nick?: string;
    };
    return cfg.self_nick;
  } catch {
    return undefined;
  }
}

function idleHtml(webview: vscode.Webview): string {
  const csp = [
    "default-src 'none'",
    `style-src ${webview.cspSource} 'unsafe-inline'`,
  ].join('; ');
  return `<!doctype html>
<html><head>
  <meta charset="utf-8">
  <meta http-equiv="Content-Security-Policy" content="${csp}">
  <style>
    body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif; padding: 24px; color: var(--vscode-foreground); background: var(--vscode-editor-background); }
    .empty { font-size: 13px; opacity: 0.7; line-height: 1.6; }
    code { font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; padding: 1px 5px; background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.12)); border-radius: 3px; }
  </style>
</head><body>
  <div class="empty">
    No Room open. Pick one from the cc-connect sidebar
    (<code>Ctrl/Cmd-Shift-P → cc-connect: Start Room</code> to mint a new one).
  </div>
</body></html>`;
}

function roomHtml(webview: vscode.Webview, distRoot: vscode.Uri): string {
  const scriptUri = webview.asWebviewUri(
    vscode.Uri.joinPath(distRoot, 'main.js'),
  );
  const csp = [
    "default-src 'none'",
    `script-src ${webview.cspSource}`,
    `style-src ${webview.cspSource} 'unsafe-inline'`,
    `font-src ${webview.cspSource}`,
  ].join('; ');

  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta http-equiv="Content-Security-Policy" content="${csp}">
  <title>cc-connect Room</title>
  <style>
    html, body { height: 100%; margin: 0; }
    body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif; color: var(--vscode-foreground); background: var(--vscode-editor-background); display: flex; flex-direction: column; font-size: 13px; }
    #root { flex: 1; display: flex; flex-direction: column; min-height: 0; }

    /* Header strip — compact meta line */
    .room-meta { font-size: 11px; opacity: 0.55; padding: 4px 10px; font-family: var(--vscode-editor-font-family, monospace); border-bottom: 1px solid var(--vscode-panel-border); flex: 0 0 auto; }

    /* Vertical split: chat top, claude bottom, 50/50 */
    .panes { flex: 1; display: grid; grid-template-rows: 1fr 1fr; min-height: 0; gap: 0; }
    .pane { display: flex; flex-direction: column; min-height: 0; border-bottom: 1px solid var(--vscode-panel-border); }
    .pane:last-child { border-bottom: none; }
    .pane-head { display: flex; align-items: center; gap: 8px; padding: 6px 10px; font-size: 11px; opacity: 0.85; font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em; flex: 0 0 auto; }
    .pane-head > span:first-child { flex: 1; }
    .head-btn { padding: 2px 6px; background: transparent; color: var(--vscode-foreground); opacity: 0.6; border: none; border-radius: 3px; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: opacity 0.12s, background 0.12s; }
    .head-btn:hover { opacity: 1; background: var(--vscode-toolbar-hoverBackground, rgba(127,127,127,0.15)); }
    .head-btn svg { display: block; }
    .pane-busy { opacity: 0.7; font-weight: 400; font-size: 10px; text-transform: none; letter-spacing: 0; }
    .muted { font-size: 12px; opacity: 0.55; padding: 8px 10px; }

    /* ========== Chat (IM-style) ========== */
    .chat-log { flex: 1; min-height: 0; padding: 4px 10px 6px; overflow-y: auto; display: flex; flex-direction: column; gap: 6px; }
    .chat-bubble { display: flex; gap: 8px; align-items: flex-end; max-width: 100%; }
    .chat-bubble.me { flex-direction: row-reverse; }
    .chat-avatar { flex: 0 0 24px; width: 24px; height: 24px; border-radius: 50%; display: flex; align-items: center; justify-content: center; font-size: 11px; font-weight: 600; color: var(--vscode-button-foreground); user-select: none; }
    .chat-content { display: flex; flex-direction: column; gap: 2px; max-width: calc(100% - 64px); min-width: 0; }
    .chat-bubble.me .chat-content { align-items: flex-end; }
    .chat-meta { font-size: 10px; opacity: 0.5; font-family: var(--vscode-editor-font-family, monospace); padding: 0 4px; }
    .chat-text { padding: 6px 10px; border-radius: 12px; line-height: 1.45; word-wrap: break-word; overflow-wrap: anywhere; background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.10)); }
    .chat-bubble.me .chat-text { background: var(--vscode-textLink-foreground); color: var(--vscode-editor-background); border-bottom-right-radius: 3px; }
    .chat-bubble.peer .chat-text { border-bottom-left-radius: 3px; }
    .mention { font-weight: 500; color: var(--vscode-textLink-foreground); }
    .chat-bubble.me .mention { color: inherit; text-decoration: underline; }
    .mention.me { background: var(--vscode-editor-selectionHighlightBackground, rgba(255,200,0,0.25)); padding: 0 3px; border-radius: 3px; }
    .mention.broadcast { font-style: italic; }

    /* Both panes' inputs share styling */
    .pane-input { position: relative; padding: 6px 10px 8px; flex: 0 0 auto; border-top: 1px solid var(--vscode-panel-border); display: flex; align-items: flex-end; gap: 6px; }
    .pane-input textarea { flex: 1; min-width: 0; box-sizing: border-box; resize: none; min-height: 32px; max-height: 140px; padding: 7px 12px; font: inherit; font-size: 12.5px; line-height: 1.45; color: var(--vscode-input-foreground); background: var(--vscode-input-background); border: 1px solid var(--vscode-input-border, transparent); border-radius: 16px; outline: none; overflow-y: auto; }
    .pane-input textarea:focus { border-color: var(--vscode-focusBorder); }
    .send-btn { flex: 0 0 auto; width: 30px; height: 30px; padding: 0; border-radius: 50%; background: var(--vscode-textLink-foreground); color: var(--vscode-editor-background); border: none; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: opacity 0.12s; }
    .send-btn:hover:not(:disabled) { background: var(--vscode-button-hoverBackground, var(--vscode-textLink-foreground)); opacity: 0.9; }
    .send-btn:disabled { opacity: 0.25; cursor: not-allowed; }
    .send-btn svg { display: block; }
    .icon-btn { flex: 0 0 auto; width: 26px; height: 26px; padding: 0; border-radius: 50%; background: transparent; color: var(--vscode-foreground); opacity: 0.55; border: none; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: opacity 0.12s, background 0.12s; }
    .icon-btn:hover { opacity: 1; background: var(--vscode-toolbar-hoverBackground, rgba(127,127,127,0.15)); }
    .icon-btn svg { display: block; }
    .slash-popup .mention-item { display: flex; gap: 8px; align-items: baseline; }
    .slash-cmd { font-family: var(--vscode-editor-font-family, monospace); color: var(--vscode-textLink-foreground); font-weight: 600; }
    .slash-label { opacity: 0.7; font-size: 11px; }
    .stop-btn { flex: 0 0 auto; width: 30px; height: 30px; padding: 0; border-radius: 50%; background: var(--vscode-errorForeground, #d04444); color: var(--vscode-editor-background); border: none; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: opacity 0.12s; }
    .stop-btn:hover { opacity: 0.85; }
    .stop-btn svg { display: block; }
    /* Mention popup — anchored above the chat input */
    .mention-popup { position: absolute; bottom: calc(100% - 4px); left: 10px; right: 10px; max-height: 160px; overflow-y: auto; background: var(--vscode-quickInput-background, var(--vscode-editorWidget-background, var(--vscode-input-background))); border: 1px solid var(--vscode-focusBorder, var(--vscode-panel-border)); border-radius: 6px; box-shadow: 0 2px 8px rgba(0,0,0,0.25); z-index: 10; padding: 2px; font-size: 12px; }
    .mention-item { padding: 4px 10px; border-radius: 4px; cursor: pointer; user-select: none; }
    .mention-item:hover { background: var(--vscode-list-hoverBackground); }
    .mention-item.selected { background: var(--vscode-list-activeSelectionBackground); color: var(--vscode-list-activeSelectionForeground); }
    /* Queue depth pill — sits between the Claude log and the input. */
    .queue-pill { margin: 4px 10px 0; padding: 4px 10px; font-size: 11px; opacity: 0.85; background: var(--vscode-editorWarning-background, rgba(255,200,0,0.10)); border-left: 2px solid var(--vscode-editorWarning-foreground, var(--vscode-textLink-foreground)); border-radius: 0 3px 3px 0; flex: 0 0 auto; }

    /* ========== Claude (agent-style) ========== */
    .claude-log { flex: 1; min-height: 0; padding: 6px 10px 6px 28px; overflow-y: auto; display: flex; flex-direction: column; gap: 4px; position: relative; }
    /* Vertical timeline connector */
    .claude-log::before { content: ''; position: absolute; left: 13px; top: 10px; bottom: 10px; width: 1px; background: var(--vscode-panel-border); }
    /* Step wrapper — each block gets a bullet on the timeline */
    .claude-step { position: relative; }
    .claude-step::before { content: ''; position: absolute; left: -18px; top: 7px; width: 8px; height: 8px; border-radius: 50%; background: var(--vscode-foreground); opacity: 0.4; z-index: 1; }
    .claude-step.ok::before { background: var(--vscode-charts-green, #6ec07b); opacity: 0.85; }
    .claude-step.done::before { background: var(--vscode-charts-green, #6ec07b); opacity: 0.55; }
    .claude-step.pending::before { background: var(--vscode-disabledForeground, #888); opacity: 0.6; animation: cc-pulse 1.4s ease-in-out infinite; }
    .claude-step.error::before { background: var(--vscode-errorForeground); opacity: 0.95; }
    .claude-row { word-wrap: break-word; }
    .claude-system { opacity: 0.45; font-size: 11px; padding: 4px 0; }
    .claude-thinking { font-size: 11px; opacity: 0.55; font-style: italic; padding: 4px 0 6px; }
    .claude-thinking.ongoing { animation: cc-pulse 1.4s ease-in-out infinite; }
    @keyframes cc-pulse { 0%, 100% { opacity: 0.4; } 50% { opacity: 0.75; } }
    .claude-hook { opacity: 0.35; font-size: 11px; padding-left: 10px; }
    .claude-text { line-height: 1.55; }
    .claude-result { opacity: 0.5; font-size: 11px; padding: 6px 0 8px; border-top: 1px dashed var(--vscode-panel-border); margin-top: 8px; }
    .claude-error { color: var(--vscode-errorForeground); }
    .claude-other { opacity: 0.3; font-size: 11px; }
    .claude-turn-sep { display: flex; align-items: center; gap: 8px; margin: 8px 0 4px; font-size: 10px; opacity: 0.45; text-transform: uppercase; letter-spacing: 0.06em; }
    .claude-turn-sep::before, .claude-turn-sep::after { content: ''; flex: 1; height: 1px; background: var(--vscode-panel-border); }

    /* Tool cards */
    .claude-tool-card { margin: 4px 0; padding: 8px 10px; border-left: 2px solid var(--vscode-textLink-foreground); background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.08)); border-radius: 0 4px 4px 0; }
    .claude-tool-card.claude-tool-error { border-left-color: var(--vscode-errorForeground); }
    .claude-tool-head { display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; font-size: 12px; }
    .claude-tool-name { font-weight: 600; color: var(--vscode-textLink-foreground); }
    .claude-tool-input { opacity: 0.85; word-break: break-all; font-family: var(--vscode-editor-font-family, monospace); font-size: 11.5px; }
    .claude-tool-pending { opacity: 0.5; margin-left: auto; }
    .claude-tool-empty { margin-top: 4px; opacity: 0.45; font-style: italic; font-size: 11px; }
    .tool-body { margin-top: 6px; }
    .tool-body-pre { font-family: var(--vscode-editor-font-family, monospace); font-size: 11px; line-height: 1.45; padding: 6px 8px; background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.10)); border-radius: 3px; overflow-x: auto; max-height: 200px; overflow-y: auto; white-space: pre; margin: 0; }
    .tool-body.tool-body-error .tool-body-pre { background: var(--vscode-inputValidation-errorBackground, rgba(255,80,80,0.10)); border: 1px solid var(--vscode-errorForeground, rgba(255,80,80,0.5)); }
    .tool-expand { margin-top: 4px; padding: 1px 8px; font-size: 11px; opacity: 0.7; background: transparent; color: var(--vscode-textLink-foreground); border: 1px solid var(--vscode-panel-border); border-radius: 2px; cursor: pointer; }
    .tool-expand:hover { opacity: 1; }
    .diff { margin-top: 6px; display: flex; flex-direction: column; gap: 4px; font-family: var(--vscode-editor-font-family, monospace); font-size: 11px; line-height: 1.45; }
    .diff-old, .diff-new { margin: 0; padding: 4px 8px; border-radius: 3px; overflow-x: auto; max-height: 180px; overflow-y: auto; white-space: pre; }
    .diff-old { background: var(--vscode-diffEditor-removedTextBackground, rgba(255,80,80,0.12)); border-left: 2px solid var(--vscode-gitDecoration-deletedResourceForeground, #d04444); }
    .diff-new { background: var(--vscode-diffEditor-insertedTextBackground, rgba(80,200,80,0.12)); border-left: 2px solid var(--vscode-gitDecoration-addedResourceForeground, #44d044); }

    /* Markdown */
    .md { font-size: 12.5px; line-height: 1.55; }
    .md > *:first-child { margin-top: 0; }
    .md > *:last-child { margin-bottom: 0; }
    .md p { margin: 4px 0; }
    .md h1, .md h2, .md h3, .md h4, .md h5, .md h6 { margin: 10px 0 4px; line-height: 1.3; font-weight: 600; }
    .md h1 { font-size: 1.18em; }
    .md h2 { font-size: 1.10em; }
    .md h3 { font-size: 1.04em; }
    .md ul, .md ol { margin: 4px 0; padding-left: 22px; }
    .md li { margin: 2px 0; }
    .md li > p { margin: 0; }
    .md blockquote { margin: 4px 0; padding: 0 12px; border-left: 3px solid var(--vscode-textBlockQuote-border, var(--vscode-panel-border)); opacity: 0.85; }
    .md a { color: var(--vscode-textLink-foreground); text-decoration: none; }
    .md a:hover { text-decoration: underline; }
    .md code { font-family: var(--vscode-editor-font-family, monospace); font-size: 0.92em; padding: 1px 5px; background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.12)); border-radius: 3px; }
    .md-pre { font-family: var(--vscode-editor-font-family, monospace); font-size: 0.92em; padding: 8px 10px; background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.12)); border-radius: 4px; overflow-x: auto; margin: 6px 0; }
    .md-pre code { padding: 0; background: transparent; border-radius: 0; font-size: 1em; }
    .md table { border-collapse: collapse; margin: 6px 0; }
    .md th, .md td { border: 1px solid var(--vscode-panel-border); padding: 4px 8px; text-align: left; }
    .md th { background: rgba(127,127,127,0.08); font-weight: 600; }
    .md hr { border: none; border-top: 1px solid var(--vscode-panel-border); margin: 10px 0; }
    .md strong { font-weight: 600; }
    .md em { font-style: italic; }
  </style>
</head>
<body>
  <div id="root"></div>
  <script type="module" src="${scriptUri}"></script>
</body>
</html>`;
}
