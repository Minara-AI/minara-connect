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
        }
      },
    );

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
    body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif; padding: 8px 12px; color: var(--vscode-foreground); background: var(--vscode-editor-background); display: flex; flex-direction: column; }
    #root { flex: 1; display: flex; flex-direction: column; min-height: 0; }
    h1 { font-size: 13px; margin: 0 0 2px; font-weight: 600; }
    .room-meta { font-size: 11px; opacity: 0.5; margin: 0 0 8px; font-family: var(--vscode-editor-font-family, monospace); }
    h2 { margin: 0 0 6px; font-size: 12px; opacity: 0.7; font-weight: 500; }
    .panes { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; flex: 1; min-height: 0; }
    .pane { display: flex; flex-direction: column; border: 1px solid var(--vscode-panel-border); border-radius: 4px; padding: 8px 10px; min-height: 0; }
    .muted { font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; opacity: 0.6; }
    .chat-log { flex: 1; min-height: 0; font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; line-height: 1.5; overflow-y: auto; }
    .chat-line { display: grid; grid-template-columns: 60px 80px 1fr; gap: 8px; padding: 1px 0; align-items: baseline; }
    .chat-line .ts { opacity: 0.4; font-variant-numeric: tabular-nums; }
    .chat-line .nick { font-weight: 600; opacity: 0.85; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .chat-line.me .nick { color: var(--vscode-textLink-foreground); }
    .chat-line .body { opacity: 0.95; word-wrap: break-word; }
    .mention { font-weight: 500; color: var(--vscode-textLink-foreground); }
    .mention.me { background: var(--vscode-editor-selectionHighlightBackground, rgba(255,200,0,0.18)); padding: 0 3px; border-radius: 2px; }
    .mention.broadcast { color: var(--vscode-symbolIcon-eventForeground, var(--vscode-textLink-foreground)); font-style: italic; }
    .chat-input { margin-top: 6px; flex: 0 0 auto; }
    .chat-input textarea { width: 100%; box-sizing: border-box; resize: vertical; min-height: 36px; max-height: 200px; padding: 6px 8px; font: inherit; font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; line-height: 1.4; color: var(--vscode-input-foreground); background: var(--vscode-input-background); border: 1px solid var(--vscode-input-border, transparent); border-radius: 3px; outline: none; }
    .chat-input textarea:focus { border-color: var(--vscode-focusBorder); }
    .claude-log { flex: 1; min-height: 0; font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; line-height: 1.55; overflow-y: auto; }
    .claude-row { padding: 2px 0; word-wrap: break-word; }
    .claude-system { opacity: 0.5; }
    .claude-hook { opacity: 0.4; padding-left: 12px; }
    .claude-text { white-space: normal; opacity: 0.95; }
    .claude-tool { color: var(--vscode-textLink-foreground); }
    .claude-result { opacity: 0.6; padding-top: 4px; border-top: 1px dashed var(--vscode-panel-border); margin-top: 4px; }
    .claude-error { color: var(--vscode-errorForeground); }
    .claude-other { opacity: 0.35; }
    .claude-tool-card { margin: 6px 0; padding: 6px 8px; border-left: 2px solid var(--vscode-textLink-foreground); background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.08)); border-radius: 0 3px 3px 0; }
    .claude-tool-card.claude-tool-error { border-left-color: var(--vscode-errorForeground); }
    .claude-tool-head { display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }
    .claude-tool-name { font-weight: 600; color: var(--vscode-textLink-foreground); }
    .claude-tool-input { opacity: 0.85; word-break: break-all; }
    .claude-tool-pending { opacity: 0.5; margin-left: auto; }
    .claude-tool-empty { margin-top: 4px; opacity: 0.45; font-style: italic; }
    .tool-body { margin-top: 6px; }
    .tool-body-pre { font-family: var(--vscode-editor-font-family, monospace); font-size: 11.5px; line-height: 1.45; padding: 6px 8px; background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.10)); border-radius: 3px; overflow-x: auto; max-height: 240px; overflow-y: auto; white-space: pre; margin: 0; }
    .tool-body.tool-body-error .tool-body-pre { background: var(--vscode-inputValidation-errorBackground, rgba(255,80,80,0.10)); border: 1px solid var(--vscode-errorForeground, rgba(255,80,80,0.5)); }
    .tool-expand { margin-top: 4px; padding: 1px 8px; font-size: 11px; opacity: 0.7; background: transparent; color: var(--vscode-textLink-foreground); border: 1px solid var(--vscode-panel-border); border-radius: 2px; cursor: pointer; }
    .tool-expand:hover { opacity: 1; }
    .diff { margin-top: 6px; display: flex; flex-direction: column; gap: 4px; font-family: var(--vscode-editor-font-family, monospace); font-size: 11.5px; line-height: 1.45; }
    .diff-old, .diff-new { margin: 0; padding: 4px 8px; border-radius: 3px; overflow-x: auto; max-height: 200px; overflow-y: auto; white-space: pre; }
    .diff-old { background: var(--vscode-diffEditor-removedTextBackground, rgba(255,80,80,0.12)); border-left: 2px solid var(--vscode-gitDecoration-deletedResourceForeground, #d04444); }
    .diff-new { background: var(--vscode-diffEditor-insertedTextBackground, rgba(80,200,80,0.12)); border-left: 2px solid var(--vscode-gitDecoration-addedResourceForeground, #44d044); }
    .pane-busy { opacity: 0.7; font-weight: 400; font-size: 11px; }
    .md { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif; font-size: 12.5px; line-height: 1.5; }
    .md > *:first-child { margin-top: 0; }
    .md > *:last-child { margin-bottom: 0; }
    .md p { margin: 4px 0; }
    .md h1, .md h2, .md h3, .md h4, .md h5, .md h6 { margin: 10px 0 4px; line-height: 1.3; font-weight: 600; }
    .md h1 { font-size: 1.25em; }
    .md h2 { font-size: 1.15em; }
    .md h3 { font-size: 1.06em; }
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
    .md hr { border: none; border-top: 1px solid var(--vscode-panel-border); margin: 12px 0; }
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
