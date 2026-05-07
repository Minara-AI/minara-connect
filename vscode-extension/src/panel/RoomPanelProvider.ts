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
import { loadLauncherPrompts } from '../host/prompts';
import {
  listSessions,
  loadSession,
  type SessionMeta,
} from '../host/transcripts';
import type { Message } from '../types';

export class RoomPanelProvider implements vscode.WebviewViewProvider {
  static readonly viewType = 'cc-connect.room';

  private view?: vscode.WebviewView;
  private currentTopic?: string;
  private myNick = '(me)';
  private tail?: LogTailHandle;
  private runner?: ClaudeRunnerHandle;
  private messageDisposable?: vscode.Disposable;
  private editorDisposable?: vscode.Disposable;

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
    this.editorDisposable?.dispose();
    this.editorDisposable = undefined;
    this.tail?.close();
    this.tail = undefined;
    this.runner?.abort();
    this.runner = undefined;
  }

  /** Push the current active editor (if any) to the webview so the
   *  Claude input can show a "ref this file" chip. Called on Room
   *  activation + every editor switch. */
  private pushActiveEditor(view: vscode.WebviewView): void {
    const ed = vscode.window.activeTextEditor;
    if (!ed || ed.document.uri.scheme !== 'file') {
      view.webview.postMessage({ type: 'editor:active', body: null });
      return;
    }
    const fsPath = ed.document.uri.fsPath;
    const basename = path.basename(fsPath);
    // Prefer a workspace-relative path so the chip stays compact and
    // peer-meaningful — `webview/Claude.tsx` reads better than
    // `/Users/.../webview/Claude.tsx`.
    const relPath = vscode.workspace.asRelativePath(fsPath, false);
    view.webview.postMessage({
      type: 'editor:active',
      body: { path: relPath, fsPath, basename },
    });
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
        } else if (msg.type === 'claude:permission-mode') {
          const mode = msg.body;
          if (
            mode === 'bypassPermissions' ||
            mode === 'acceptEdits' ||
            mode === 'plan' ||
            mode === 'default'
          ) {
            this.runner?.setPermissionMode(mode);
          }
        } else if (msg.type === 'claude:permission-response') {
          const b = msg.body as
            | {
                requestId?: string;
                behavior?: 'allow' | 'deny' | 'always-allow';
              }
            | undefined;
          if (
            b &&
            typeof b.requestId === 'string' &&
            (b.behavior === 'allow' ||
              b.behavior === 'deny' ||
              b.behavior === 'always-allow')
          ) {
            this.runner?.resolvePermission(b.requestId, b.behavior);
          }
        } else if (msg.type === 'history:list') {
          // List Claude transcripts for the current workspace cwd.
          // No room filter — Claude transcripts predate cc-connect's
          // Room concept, and we want users to see everything they've
          // run in this folder.
          const cwd = workspaceCwd();
          const sessions: SessionMeta[] = cwd ? listSessions(cwd) : [];
          view.webview.postMessage({
            type: 'history:list-result',
            body: sessions.map((s) => ({
              sessionId: s.sessionId,
              firstPrompt: s.firstPrompt,
              mtimeMs: s.mtimeMs,
              messageCount: s.messageCount,
            })),
          });
        } else if (msg.type === 'history:load') {
          const sid = typeof msg.body === 'string' ? msg.body : '';
          const cwd = workspaceCwd();
          if (!sid || !cwd) return;
          try {
            const events = loadSession(cwd, sid);
            view.webview.postMessage({
              type: 'history:loaded',
              body: { sessionId: sid, events },
            });
          } catch (e) {
            view.webview.postMessage({
              type: 'history:loaded',
              body: {
                sessionId: sid,
                events: [],
                error: e instanceof Error ? e.message : String(e),
              },
            });
          }
        } else if (msg.type === 'prompt:open-file') {
          // File-ref chip clicked in the Claude prompt log. Resolve
          // relative paths against the workspace root, expand `~`,
          // then open in the editor. Failures fall through silently
          // — chips are heuristic and may catch non-paths.
          const raw = typeof msg.body === 'string' ? msg.body.trim() : '';
          if (!raw) return;
          const resolved = resolveFileRef(raw);
          if (!resolved) return;
          try {
            const doc = await vscode.workspace.openTextDocument(resolved);
            await vscode.window.showTextDocument(doc, { preview: true });
          } catch {
            // Not a real file, or no permission — silent.
          }
        } else if (msg.type === 'room:copy-ticket') {
          // Read the ticket fresh from chat-daemon.pid (JSON); the
          // host-bg + chat-daemon both write it there at startup.
          // Avoid threading it through webview state — peers' Tickets
          // are capabilities, keep the surface area small.
          const ticket = readRoomTicket(t);
          if (!ticket) {
            void vscode.window.showWarningMessage(
              'cc-connect: no ticket on disk for this Room. Was it started here?',
            );
            return;
          }
          await vscode.env.clipboard.writeText(ticket);
          void vscode.window.showInformationMessage(
            'cc-connect: Ticket copied to clipboard.',
          );
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
        } else if (msg.type === 'chat:paste-files') {
          // Webview pasted one or more File items (screenshot, dragged
          // image, etc.). Webview is sandboxed, so it ferried the
          // bytes as base64. Materialize them under os.tmpdir() and
          // hand the path to ccDrop — chat-daemon hashes + persists
          // into iroh-blobs from there.
          const files = Array.isArray(msg.body)
            ? (msg.body as { name?: unknown; dataB64?: unknown }[])
            : [];
          const scratchDir = path.join(
            os.tmpdir(),
            `cc-connect-paste-${process.pid}`,
          );
          try {
            fs.mkdirSync(scratchDir, { recursive: true, mode: 0o700 });
          } catch {
            // best-effort; if mkdir fails the writes below will too.
          }
          for (const f of files) {
            const name = typeof f.name === 'string' && f.name ? f.name : 'pasted-file';
            const dataB64 = typeof f.dataB64 === 'string' ? f.dataB64 : '';
            if (!dataB64) continue;
            const safeName = path.basename(name).replace(/[^\w.\-]/g, '_');
            const tmpPath = path.join(
              scratchDir,
              `${Date.now()}-${Math.random().toString(36).slice(2, 8)}-${safeName}`,
            );
            try {
              fs.writeFileSync(tmpPath, Buffer.from(dataB64, 'base64'));
            } catch (e) {
              view.webview.postMessage({
                type: 'chat:send-error',
                body: `paste write failed: ${e instanceof Error ? e.message : String(e)}`,
              });
              continue;
            }
            const resp = await ccDrop(t, tmpPath);
            if (!resp.ok) {
              view.webview.postMessage({
                type: 'chat:send-error',
                body: resp.err ?? 'unknown ipc error',
              });
            }
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

    const launcherPrompts = loadLauncherPrompts(
      this.context.extensionUri.fsPath,
    );
    // Resume the prior Session for this Room if we have one stored.
    // Persisted via globalState (per-machine, survives VSCode reloads
    // and panel close/reopen). Cleanup goes with the extension itself
    // — uninstalling the .vsix wipes globalState, so no extra entry
    // for the lifecycle.rs cleanup contract.
    const sessionKey = sessionStateKey(topic);
    const resumeSessionId = this.context.globalState.get<string>(sessionKey);
    this.runner = createClaudeRunner({
      topic,
      // Same prompt pair the TUI feeds claude via `claude-wrap.sh`:
      // - autoReply → `--append-system-prompt` (Claude learns it's
      //   in a cc-connect Room, learns the cc_* MCP tools)
      // - bootstrap → first user prompt (Claude greets + enters the
      //   `cc_wait_for_mention` listener loop). Suppressed inside the
      //   runner when resuming.
      systemPromptAppend: launcherPrompts.autoReply,
      initialPrompt: launcherPrompts.bootstrap,
      resumeSessionId,
      onSessionId: (sid) => {
        void this.context.globalState.update(sessionKey, sid);
      },
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

    // Active-editor tracking: seed once + watch for switches. The
    // webview shows a "ref this file" chip in the Claude input.
    this.pushActiveEditor(view);
    this.editorDisposable = vscode.window.onDidChangeActiveTextEditor(() =>
      this.pushActiveEditor(view),
    );

    view.webview.postMessage({ type: 'host:ready' });
  }
}

/** globalState key for the most-recent Claude sessionId we used in
 *  this Room. Per-topic so different Rooms don't trample each other.
 *  Lives in VSCode's storage, so cleanup goes with the extension. */
function sessionStateKey(topic: string): string {
  return `cc-connect.session.${topic}`;
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

function readRoomTicket(topic: string): string | undefined {
  // Both host-bg and chat-daemon write a JSON pid-file at
  // ~/.cc-connect/rooms/<topic>/chat-daemon.pid containing
  // { pid, topic, ticket, started_at, relay, no_relay }.
  // We just want the ticket — same Ticket the TUI prints + the
  // clipboard-copy on `cc-connect room start`.
  const fp = path.join(
    os.homedir(),
    '.cc-connect',
    'rooms',
    topic,
    'chat-daemon.pid',
  );
  try {
    const raw = fs.readFileSync(fp, 'utf8');
    const parsed = JSON.parse(raw) as { ticket?: string };
    return typeof parsed.ticket === 'string' && parsed.ticket.startsWith('cc1-')
      ? parsed.ticket
      : undefined;
  } catch {
    return undefined;
  }
}

function workspaceCwd(): string | undefined {
  // Use the first workspace folder as the project root for Claude
  // transcripts. Multi-root workspaces fall back to the first folder
  // — Claude itself uses the cwd `claude` was invoked in.
  const folders = vscode.workspace.workspaceFolders;
  return folders?.[0]?.uri.fsPath;
}

function resolveFileRef(raw: string): vscode.Uri | undefined {
  let p = raw;
  if (p.startsWith('~/')) p = path.join(os.homedir(), p.slice(2));
  if (path.isAbsolute(p)) {
    return fs.existsSync(p) ? vscode.Uri.file(p) : undefined;
  }
  // Relative — try every workspace folder root.
  const folders = vscode.workspace.workspaceFolders ?? [];
  for (const f of folders) {
    const abs = path.join(f.uri.fsPath, p);
    if (fs.existsSync(abs)) return vscode.Uri.file(abs);
  }
  return undefined;
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
  const codiconCssUri = webview.asWebviewUri(
    vscode.Uri.joinPath(distRoot, 'codicon.css'),
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
  <link rel="stylesheet" href="${codiconCssUri}">
  <style>
    html, body { height: 100%; margin: 0; }
    body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif; color: var(--vscode-foreground); background: var(--vscode-editor-background); display: flex; flex-direction: column; font-size: 13px; }
    #root { flex: 1; display: flex; flex-direction: column; min-height: 0; }

    /* Header strip — three segments: topic, nick, status */
    .room-meta { display: flex; gap: 10px; align-items: center; font-size: 10.5px; padding: 4px 10px; font-family: var(--vscode-editor-font-family, monospace); border-bottom: 1px solid var(--vscode-panel-border); flex: 0 0 auto; letter-spacing: 0.01em; }
    .room-meta-topic { color: var(--vscode-textLink-foreground); opacity: 0.9; }
    .room-meta-nick { opacity: 0.85; }
    .room-meta-status { margin-left: auto; opacity: 0.55; }
    .room-meta-copy { display: inline-flex; align-items: center; gap: 3px; padding: 2px 8px; border-radius: 10px; background: rgba(95,168,211,0.14); color: var(--vscode-charts-blue, #5fa8d3); border: 1px solid rgba(95,168,211,0.32); font-size: 10.5px; line-height: 1.5; cursor: pointer; font-family: var(--vscode-editor-font-family, monospace); }
    .room-meta-copy:hover { background: rgba(95,168,211,0.26); }
    .room-meta-copy .codicon { font-size: 11px; }

    /* Tab strip — single active tab fills the pane below */
    .tab-strip { display: flex; align-items: stretch; border-bottom: 1px solid var(--vscode-panel-border); flex: 0 0 auto; background: var(--vscode-editor-background); }
    .tab { display: flex; align-items: center; gap: 6px; padding: 9px 14px; font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.06em; color: var(--vscode-foreground); opacity: 0.5; background: transparent; border: none; border-bottom: 2px solid transparent; cursor: pointer; transition: opacity 0.12s, border-color 0.12s, color 0.12s; }
    .tab:hover { opacity: 0.8; }
    .tab.active { opacity: 1; border-bottom-color: var(--vscode-textLink-foreground); color: var(--vscode-textLink-foreground); }
    .tab .codicon { font-size: 14px; }
    .tab-badge { background: var(--vscode-textLink-foreground); color: var(--vscode-editor-background); font-size: 9px; font-weight: 700; padding: 1px 5px; border-radius: 6px; line-height: 1.4; min-width: 12px; text-align: center; letter-spacing: 0; }
    .tab-busy { font-size: 12px; opacity: 0.7; }
    .codicon-modifier-spin { animation: codicon-spin 1.2s linear infinite; }
    @keyframes codicon-spin { to { transform: rotate(360deg); } }

    /* Tab pane swap — keep both mounted, hide the inactive one */
    .panes { flex: 1; display: flex; flex-direction: column; min-height: 0; }
    .pane-wrap { flex: 1; min-height: 0; display: flex; flex-direction: column; }
    .pane-wrap.hidden { display: none; }
    .pane { display: flex; flex-direction: column; min-height: 0; flex: 1; }
    .pane-head { display: flex; align-items: center; gap: 8px; padding: 4px 10px; font-size: 10.5px; opacity: 0.6; font-weight: 600; text-transform: uppercase; letter-spacing: 0.06em; flex: 0 0 auto; border-bottom: 1px solid var(--vscode-panel-border); }
    .pane-head > span:first-child { flex: 1; }
    .pane-head-actions { display: flex; gap: 2px; align-items: center; }
    .head-btn { padding: 3px 6px; background: transparent; color: var(--vscode-foreground); opacity: 0.6; border: none; border-radius: 3px; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: opacity 0.12s, background 0.12s; }
    .head-btn:hover { opacity: 1; background: var(--vscode-toolbar-hoverBackground, rgba(127,127,127,0.15)); }
    .head-btn.active { opacity: 1; background: var(--vscode-toolbar-activeBackground, rgba(95,168,211,0.2)); color: var(--vscode-charts-blue, #5fa8d3); }
    .head-btn .codicon { font-size: 14px; }
    .pane-busy { opacity: 0.7; font-weight: 400; font-size: 10px; text-transform: none; letter-spacing: 0; }

    /* History picker overlay + viewing banner */
    .history-picker { display: flex; flex-direction: column; max-height: 320px; flex: 0 0 auto; background: var(--vscode-sideBar-background); border-bottom: 1px solid var(--vscode-panel-border); }
    .history-picker-head { display: flex; align-items: center; justify-content: space-between; padding: 4px 10px; font-size: 10.5px; opacity: 0.55; font-weight: 600; text-transform: uppercase; letter-spacing: 0.06em; border-bottom: 1px solid var(--vscode-panel-border); }
    .history-list { overflow-y: auto; padding: 2px 0; }
    .history-list .muted { padding: 12px 10px; font-size: 11.5px; opacity: 0.5; font-style: italic; text-align: center; }
    .history-item { display: block; width: 100%; text-align: left; padding: 6px 10px; background: transparent; border: none; border-bottom: 1px solid rgba(127,127,127,0.10); color: var(--vscode-foreground); cursor: pointer; font: inherit; transition: background 0.1s; }
    .history-item:last-child { border-bottom: none; }
    .history-item:hover { background: var(--vscode-list-hoverBackground, rgba(127,127,127,0.08)); }
    .history-item.active { background: var(--vscode-list-activeSelectionBackground, rgba(95,168,211,0.18)); }
    .history-item-title { font-size: 12px; line-height: 1.4; word-break: break-word; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; margin-bottom: 2px; }
    .history-item-meta { display: flex; gap: 5px; font-size: 10.5px; opacity: 0.5; }
    .history-item-sid { font-family: var(--vscode-editor-font-family, monospace); }
    .history-banner { display: flex; align-items: center; gap: 8px; padding: 5px 10px; background: rgba(95,168,211,0.10); border-bottom: 1px solid rgba(95,168,211,0.32); font-size: 11.5px; color: var(--vscode-charts-blue, #5fa8d3); flex: 0 0 auto; }
    .history-banner .codicon { font-size: 13px; opacity: 0.75; }
    .history-banner-exit { margin-left: auto; padding: 1px 8px; background: transparent; color: var(--vscode-charts-blue, #5fa8d3); border: 1px solid rgba(95,168,211,0.5); border-radius: 3px; font-size: 11px; cursor: pointer; }
    .history-banner-exit:hover { background: rgba(95,168,211,0.18); }
    .muted { font-size: 12px; opacity: 0.55; padding: 8px 10px; }
    .muted-empty { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 6px; padding: 24px 10px; opacity: 0.4; font-size: 11px; flex: 1; }
    .muted-empty .codicon { font-size: 28px; opacity: 0.5; }

    /* ========== Chat (own messages right-aligned, peers left) ========== */
    .chat-log { flex: 1; min-height: 0; padding: 6px 10px 8px; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; }
    .chat-row { display: flex; gap: 8px; align-items: flex-start; animation: cc-fade-in 0.18s ease-out; }
    .chat-row.me { flex-direction: row-reverse; }
    .chat-row.continuation { gap: 8px; margin-top: -6px; }
    .chat-avatar { flex: 0 0 22px; width: 22px; height: 22px; border-radius: 4px; display: flex; align-items: center; justify-content: center; font-size: 10.5px; font-weight: 700; color: var(--vscode-editor-background); user-select: none; line-height: 1; }
    .chat-avatar-spacer { flex: 0 0 22px; }
    .chat-body { flex: 1 1 auto; min-width: 0; max-width: calc(100% - 30px); }
    .chat-row.me .chat-body { display: flex; flex-direction: column; align-items: flex-end; }
    .chat-byline { display: flex; align-items: baseline; gap: 8px; font-size: 11px; line-height: 1.2; }
    .chat-row.me .chat-byline { flex-direction: row-reverse; }
    .chat-nick { font-weight: 600; color: var(--vscode-foreground); }
    .chat-row.me .chat-nick { color: var(--vscode-textLink-foreground); }
    .chat-ts { font-size: 10px; opacity: 0.4; font-family: var(--vscode-editor-font-family, monospace); font-variant-numeric: tabular-nums; }
    .chat-text { font-size: 12.5px; line-height: 1.5; word-wrap: break-word; overflow-wrap: anywhere; padding-top: 1px; max-width: 100%; }
    .chat-row.me .chat-text { background: var(--vscode-charts-blue, rgba(95,168,211,0.18)); color: var(--vscode-foreground); padding: 4px 10px; border-radius: 12px 12px 4px 12px; background: rgba(95,168,211,0.18); display: inline-block; max-width: 100%; }
    .chat-row.peer .chat-text { background: rgba(127,127,127,0.10); padding: 4px 10px; border-radius: 12px 12px 12px 4px; display: inline-block; max-width: 100%; }
    .chat-row.continuation .chat-text { border-top-left-radius: 4px; border-top-right-radius: 4px; }
    .mention { font-weight: 500; color: var(--vscode-textLink-foreground); }
    .mention.me { background: var(--vscode-editor-selectionHighlightBackground, rgba(255,200,0,0.22)); padding: 0 3px; border-radius: 3px; font-weight: 600; }
    .mention.broadcast { font-style: italic; }
    @keyframes cc-fade-in { from { opacity: 0; transform: translateY(2px); } to { opacity: 1; transform: none; } }

    /* Both panes' inputs share styling */
    .pane-input-ref { display: flex; align-items: center; gap: 6px; padding: 4px 10px 0; flex: 0 0 auto; }
    .editor-ref-chip { display: inline-flex; align-items: center; gap: 4px; padding: 1px 8px; border-radius: 10px; background: var(--vscode-badge-background); color: var(--vscode-badge-foreground); border: 1px solid var(--vscode-button-border, transparent); font-size: 11px; cursor: pointer; line-height: 1.6; font-family: var(--vscode-editor-font-family, monospace); }
    .editor-ref-chip:hover { background: var(--vscode-list-activeSelectionBackground, var(--vscode-button-hoverBackground)); }
    .editor-ref-chip .codicon { font-size: 11px; opacity: 0.85; }
    .editor-ref-hint { font-size: 10.5px; opacity: 0.45; }
    .pane-input { position: relative; padding: 6px 10px 8px; flex: 0 0 auto; border-top: 1px solid var(--vscode-panel-border); display: flex; align-items: flex-end; gap: 6px; }
    .mode-pill { display: inline-flex; align-items: center; gap: 4px; padding: 3px 9px; border-radius: 12px; font-size: 11px; line-height: 1; cursor: pointer; border: 1px solid var(--vscode-button-border, transparent); background: var(--vscode-button-secondaryBackground, rgba(127,127,127,0.18)); color: var(--vscode-button-secondaryForeground, var(--vscode-foreground)); transition: background 0.12s, border-color 0.12s; align-self: center; }
    .mode-pill:hover { background: var(--vscode-button-secondaryHoverBackground, rgba(127,127,127,0.28)); }
    .mode-pill .codicon { font-size: 11px; }
    .mode-pill.mode-bypassPermissions { background: rgba(95,168,211,0.18); color: var(--vscode-charts-blue, #5fa8d3); border-color: rgba(95,168,211,0.4); }
    .mode-pill.mode-bypassPermissions:hover { background: rgba(95,168,211,0.30); }
    .mode-pill.mode-acceptEdits { background: rgba(214,168,83,0.18); color: var(--vscode-charts-yellow, #d6a853); border-color: rgba(214,168,83,0.4); }
    .mode-pill.mode-acceptEdits:hover { background: rgba(214,168,83,0.30); }
    .mode-pill.mode-plan { background: rgba(110,192,123,0.18); color: var(--vscode-charts-green, #6ec07b); border-color: rgba(110,192,123,0.4); }
    .mode-pill.mode-plan:hover { background: rgba(110,192,123,0.30); }
    .mode-pill.mode-default { background: rgba(255,165,80,0.18); color: var(--vscode-charts-orange, #d59155); border-color: rgba(255,165,80,0.4); }
    .mode-pill.mode-default:hover { background: rgba(255,165,80,0.30); }

    /* Permission bubble — inline approval prompt for default mode */
    .permission-bubble { margin: 4px 0; padding: 8px 10px; border: 1px solid var(--vscode-inputValidation-warningBorder, var(--vscode-charts-orange, #d59155)); background: var(--vscode-inputValidation-warningBackground, rgba(214,168,83,0.10)); border-radius: 4px; font-size: 12px; }
    .permission-bubble.permission-allowed { border-color: var(--vscode-charts-green, #6ec07b); background: rgba(110,192,123,0.08); opacity: 0.75; }
    .permission-bubble.permission-always-allowed { border-color: var(--vscode-charts-blue, #5fa8d3); background: rgba(95,168,211,0.08); opacity: 0.75; }
    .permission-bubble.permission-denied { border-color: var(--vscode-errorForeground); background: rgba(255,80,80,0.08); opacity: 0.75; }
    .permission-bubble-head { display: flex; align-items: center; gap: 6px; }
    .permission-bubble-head .codicon { font-size: 13px; opacity: 0.9; }
    .permission-bubble-title { flex: 1; font-weight: 600; word-break: break-word; }
    .permission-bubble-state { font-size: 10.5px; padding: 1px 6px; border-radius: 8px; background: rgba(127,127,127,0.18); text-transform: uppercase; letter-spacing: 0.06em; }
    .permission-bubble.permission-allowed .permission-bubble-state { background: rgba(110,192,123,0.22); color: var(--vscode-charts-green, #6ec07b); }
    .permission-bubble.permission-always-allowed .permission-bubble-state { background: rgba(95,168,211,0.22); color: var(--vscode-charts-blue, #5fa8d3); }
    .permission-bubble.permission-denied .permission-bubble-state { background: rgba(255,80,80,0.22); color: var(--vscode-errorForeground); }
    .permission-bubble-ts { font-size: 10.5px; opacity: 0.45; font-family: var(--vscode-editor-font-family, monospace); font-variant-numeric: tabular-nums; flex: 0 0 auto; }
    .permission-bubble-desc { margin-top: 4px; font-size: 11.5px; opacity: 0.75; line-height: 1.45; }
    .permission-bubble-summary { margin-top: 4px; font-size: 11.5px; font-family: var(--vscode-editor-font-family, monospace); opacity: 0.85; word-break: break-all; }
    .permission-bubble-meta { margin-top: 3px; font-size: 11px; display: flex; gap: 6px; align-items: baseline; opacity: 0.7; }
    .permission-bubble-meta code { font-family: var(--vscode-editor-font-family, monospace); font-size: 11px; background: var(--vscode-textCodeBlock-background); padding: 1px 4px; border-radius: 2px; word-break: break-all; }
    .permission-bubble-actions { display: flex; gap: 6px; margin-top: 8px; justify-content: flex-end; }
    .permission-btn { display: inline-flex; align-items: center; gap: 4px; padding: 3px 12px; border-radius: 3px; font-size: 11.5px; cursor: pointer; border: 1px solid var(--vscode-button-border, transparent); }
    .permission-btn .codicon { font-size: 12px; }
    .permission-btn-allow { background: var(--vscode-button-background); color: var(--vscode-button-foreground); }
    .permission-btn-allow:hover { background: var(--vscode-button-hoverBackground); }
    .permission-btn-deny { background: var(--vscode-button-secondaryBackground, rgba(127,127,127,0.18)); color: var(--vscode-button-secondaryForeground, var(--vscode-foreground)); }
    .permission-btn-deny:hover { background: var(--vscode-button-secondaryHoverBackground, rgba(127,127,127,0.30)); }
    .permission-btn-always { background: rgba(95,168,211,0.16); color: var(--vscode-charts-blue, #5fa8d3); border-color: rgba(95,168,211,0.4); }
    .permission-btn-always:hover { background: rgba(95,168,211,0.28); }
    .pane-input textarea { flex: 1; min-width: 0; box-sizing: border-box; resize: none; min-height: 32px; max-height: 140px; padding: 7px 12px; font: inherit; font-size: 12.5px; line-height: 1.45; color: var(--vscode-input-foreground); background: var(--vscode-input-background); border: 1px solid var(--vscode-input-border, transparent); border-radius: 16px; outline: none; overflow-y: auto; }
    .pane-input textarea:focus { border-color: var(--vscode-focusBorder); }
    .pane-input textarea:disabled { opacity: 0.55; cursor: not-allowed; background: var(--vscode-input-background); }
    .send-btn { flex: 0 0 auto; width: 30px; height: 30px; padding: 0; border-radius: 50%; background: var(--vscode-textLink-foreground); color: var(--vscode-editor-background); border: none; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: opacity 0.12s; }
    .send-btn:hover:not(:disabled) { background: var(--vscode-button-hoverBackground, var(--vscode-textLink-foreground)); opacity: 0.9; }
    .send-btn:disabled { opacity: 0.25; cursor: not-allowed; }
    .send-btn .codicon { font-size: 14px; }
    .icon-btn { flex: 0 0 auto; width: 28px; height: 28px; padding: 0; border-radius: 4px; background: transparent; color: var(--vscode-foreground); opacity: 0.55; border: none; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: opacity 0.12s, background 0.12s; }
    .icon-btn:hover { opacity: 1; background: var(--vscode-toolbar-hoverBackground, rgba(127,127,127,0.15)); }
    .icon-btn .codicon { font-size: 15px; }
    .slash-popup .mention-item { display: flex; gap: 8px; align-items: baseline; }
    .slash-cmd { font-family: var(--vscode-editor-font-family, monospace); color: var(--vscode-textLink-foreground); font-weight: 600; }
    .slash-label { opacity: 0.7; font-size: 11px; }
    .stop-btn { flex: 0 0 auto; width: 30px; height: 30px; padding: 0; border-radius: 50%; background: var(--vscode-errorForeground, #d04444); color: var(--vscode-editor-background); border: none; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: opacity 0.12s; }
    .stop-btn:hover { opacity: 0.85; }
    .stop-btn .codicon { font-size: 13px; }
    /* Mention popup — anchored above the chat input */
    .mention-popup { position: absolute; bottom: calc(100% - 4px); left: 10px; right: 10px; max-height: 160px; overflow-y: auto; background: var(--vscode-quickInput-background, var(--vscode-editorWidget-background, var(--vscode-input-background))); border: 1px solid var(--vscode-focusBorder, var(--vscode-panel-border)); border-radius: 6px; box-shadow: 0 2px 8px rgba(0,0,0,0.25); z-index: 10; padding: 2px; font-size: 12px; }
    .mention-item { padding: 4px 10px; border-radius: 4px; cursor: pointer; user-select: none; }
    .mention-item:hover { background: var(--vscode-list-hoverBackground); }
    .mention-item.selected { background: var(--vscode-list-activeSelectionBackground); color: var(--vscode-list-activeSelectionForeground); }
    /* Queue depth pill — sits between the Claude log and the input. */
    .queue-pill { margin: 4px 10px 0; padding: 4px 10px; font-size: 11px; opacity: 0.85; background: var(--vscode-editorWarning-background, rgba(255,200,0,0.10)); border-left: 2px solid var(--vscode-editorWarning-foreground, var(--vscode-textLink-foreground)); border-radius: 0 3px 3px 0; flex: 0 0 auto; }

    /* ========== Claude (agent-style) ========== */
    .claude-log { flex: 1; min-height: 0; padding: 6px 10px; overflow-y: auto; display: flex; flex-direction: column; gap: 6px; position: relative; }
    .claude-flat { /* plain text, prompts, results — no timeline bullet */ }
    /* Vertical timeline connector */
    /* Removed the full-height vertical line — only step blocks (tool /
       thinking / hook) carry a bullet now. Plain text + prompts read
       like a normal chat instead of a process trace. */
    /* Step wrapper — each block gets a bullet on the timeline */
    .claude-step { position: relative; padding-left: 18px; }
    .claude-step::before { content: ''; position: absolute; left: 4px; top: 8px; width: 7px; height: 7px; border-radius: 50%; background: var(--vscode-foreground); opacity: 0.4; z-index: 1; }
    .claude-step.ok::before { background: var(--vscode-charts-green, #6ec07b); opacity: 0.85; }
    .claude-step.done::before { background: var(--vscode-charts-green, #6ec07b); opacity: 0.55; }
    .claude-step.pending::before { background: var(--vscode-disabledForeground, #888); opacity: 0.6; animation: cc-pulse 1.4s ease-in-out infinite; }
    .claude-step.error::before { background: var(--vscode-errorForeground); opacity: 0.95; }
    .claude-step.me::before { background: var(--vscode-charts-blue, #5fa8d3); opacity: 0.9; }
    .claude-row { word-wrap: break-word; }
    .claude-prompt { display: flex; gap: 6px; align-items: flex-start; padding: 4px 0; font-size: 12.5px; line-height: 1.55; }
    .claude-prompt-arrow { color: var(--vscode-charts-blue, #5fa8d3); font-weight: 700; opacity: 0.85; flex: 0 0 auto; }
    .claude-prompt-body { flex: 1 1 auto; min-width: 0; word-break: break-word; }
    .file-chip { display: inline-flex; align-items: center; gap: 3px; vertical-align: baseline; margin: 0 2px; padding: 0 6px 0 4px; border-radius: 10px; background: rgba(95,168,211,0.14); color: var(--vscode-textLink-foreground); border: 1px solid rgba(95,168,211,0.32); font-size: 11px; font-family: var(--vscode-editor-font-family, monospace); cursor: pointer; line-height: 1.5; }
    .file-chip:hover { background: rgba(95,168,211,0.26); border-color: rgba(95,168,211,0.5); }
    .file-chip .codicon { font-size: 11px; opacity: 0.75; }
    .file-chip-name { white-space: nowrap; }
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

    /* Tool cards — VSCode-native styling, IN/OUT split */
    .claude-tool-card { margin: 2px 0; padding: 0; border: 1px solid var(--vscode-widget-border, var(--vscode-panel-border)); background: var(--vscode-editorWidget-background, var(--vscode-sideBar-background)); border-radius: 4px; overflow: hidden; font-size: 11.5px; }
    .claude-tool-card.claude-tool-error { border-color: var(--vscode-inputValidation-errorBorder, var(--vscode-errorForeground)); }
    .claude-tool-head { display: flex; gap: 6px; align-items: center; padding: 3px 8px; font-size: 11.5px; background: var(--vscode-editorGroupHeader-tabsBackground, var(--vscode-titleBar-inactiveBackground, transparent)); border-bottom: 1px solid var(--vscode-widget-border, var(--vscode-panel-border)); }
    .claude-tool-head .codicon { font-size: 12px; opacity: 0.7; }
    .claude-tool-name { font-weight: 600; color: var(--vscode-foreground); letter-spacing: 0; }
    .claude-tool-pending { opacity: 0.5; margin-left: auto; font-size: 11px; }
    .claude-tool-block { display: flex; gap: 6px; align-items: flex-start; padding: 3px 8px; }
    .claude-tool-block + .claude-tool-block { border-top: 1px solid var(--vscode-widget-border, var(--vscode-panel-border)); }
    .claude-tool-out { background: var(--vscode-textBlockQuote-background, transparent); }
    .claude-tool-label { flex: 0 0 auto; font-size: 9px; font-weight: 700; letter-spacing: 0.6px; padding: 1px 4px; border-radius: 2px; background: var(--vscode-badge-background); color: var(--vscode-badge-foreground); margin-top: 2px; opacity: 0.7; }
    .claude-tool-error .claude-tool-out .claude-tool-label { background: var(--vscode-inputValidation-errorBackground); color: var(--vscode-errorForeground); opacity: 1; }
    .claude-tool-input { opacity: 0.95; word-break: break-all; font-family: var(--vscode-editor-font-family, monospace); font-size: 11.5px; flex: 1 1 auto; min-width: 0; padding-top: 1px; color: var(--vscode-descriptionForeground); }
    .claude-tool-out-body { flex: 1 1 auto; min-width: 0; }
    .claude-tool-pending-out { opacity: 0.55; font-style: italic; font-size: 11px; }
    .claude-tool-empty { opacity: 0.45; font-style: italic; font-size: 11px; }
    .tool-body { margin: 0; }
    /* No horizontal padding on the result <pre> so its first character
       lines up with IN's first character — both sit right after the
       parent flex gap. The bg color band still works visually. */
    .tool-body-pre { font-family: var(--vscode-editor-font-family, monospace); font-size: 11px; line-height: 1.45; padding: 2px 0; background: transparent; border-radius: 0; overflow-x: auto; max-height: 200px; overflow-y: auto; white-space: pre; margin: 0; color: var(--vscode-editor-foreground); }
    .tool-body.tool-body-error .tool-body-pre { background: var(--vscode-inputValidation-errorBackground); border-radius: 2px; padding: 2px 6px; border: 1px solid var(--vscode-inputValidation-errorBorder, var(--vscode-errorForeground)); color: var(--vscode-errorForeground); }
    .tool-expand { margin-top: 4px; padding: 1px 8px; font-size: 11px; background: transparent; color: var(--vscode-textLink-foreground); border: 1px solid var(--vscode-button-border, transparent); border-radius: 2px; cursor: pointer; }
    .tool-expand:hover { background: var(--vscode-toolbar-hoverBackground); }
    .diff { margin: 0; display: flex; flex-direction: column; gap: 2px; font-family: var(--vscode-editor-font-family, monospace); font-size: 11px; line-height: 1.45; }
    .diff-old, .diff-new { margin: 0; padding: 3px 6px; border-radius: 2px; overflow-x: auto; max-height: 180px; overflow-y: auto; white-space: pre; }
    .diff-old { background: var(--vscode-diffEditor-removedLineBackground, var(--vscode-diffEditor-removedTextBackground)); color: var(--vscode-foreground); }
    .diff-new { background: var(--vscode-diffEditor-insertedLineBackground, var(--vscode-diffEditor-insertedTextBackground)); color: var(--vscode-foreground); }

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
