import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as vscode from 'vscode';
import { createClaudeRunner } from './host/claude_runner';
import {
  startChatDaemon,
  startHostBg,
  stopChatDaemon,
  stopHostBg,
} from './host/daemon';
import { ccDrop, ccSend } from './host/ipc';
import { tailLog, type LogTailHandle } from './host/log_tail';
import { shouldWakeClaude } from './host/mention';
import { RoomsProvider } from './sidebar/RoomsProvider';
import type { Message } from './types';

let roomsProvider: RoomsProvider | undefined;

export function activate(context: vscode.ExtensionContext): void {
  roomsProvider = new RoomsProvider();
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider('cc-connect.rooms', roomsProvider),
    vscode.commands.registerCommand('cc-connect.hello', () => {
      vscode.window.showInformationMessage('cc-connect: hello');
    }),
    vscode.commands.registerCommand(
      'cc-connect.openRoom',
      (arg?: string | { topic: string }) => {
        const topic = resolveTopicArg(arg);
        void openRoom(context, topic);
      },
    ),
    vscode.commands.registerCommand('cc-connect.startRoom', () => {
      void startRoom(context);
    }),
    vscode.commands.registerCommand('cc-connect.joinRoom', () => {
      void joinRoom(context);
    }),
    vscode.commands.registerCommand(
      'cc-connect.showTicket',
      (arg?: string | { topic: string }) => {
        void showTicket(resolveTopicArg(arg));
      },
    ),
    vscode.commands.registerCommand(
      'cc-connect.stopRoom',
      (arg?: string | { topic: string }) => {
        void stopRoom(resolveTopicArg(arg));
      },
    ),
    vscode.commands.registerCommand('cc-connect.refreshRooms', () => {
      roomsProvider?.refresh();
    }),
  );
}

export function deactivate(): void {}

/** Commands invoked from the tree get either a string topic (when the
 *  TreeItem.command.arguments is set explicitly) or the tree entry
 *  itself (for context-menu actions). Normalise both into a topic
 *  string, or undefined when invoked from the command palette. */
function resolveTopicArg(
  arg: string | { topic: string } | undefined,
): string | undefined {
  if (typeof arg === 'string') return arg;
  if (arg && typeof arg === 'object' && typeof arg.topic === 'string') {
    return arg.topic;
  }
  return undefined;
}

async function openRoom(
  context: vscode.ExtensionContext,
  topic: string | undefined,
): Promise<void> {
  const t = topic ?? (await pickTopic());
  if (!t) return;
  openRoomPanelForTopic(context, t);
}

async function startRoom(context: vscode.ExtensionContext): Promise<void> {
  let ticket: string;
  let topic: string;
  try {
    await vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: 'cc-connect: starting Room…',
        cancellable: false,
      },
      async () => {
        ticket = await startHostBg();
        topic = await startChatDaemon(ticket);
      },
    );
  } catch (e) {
    void vscode.window.showErrorMessage(
      `cc-connect: failed to start Room — ${(e as Error).message}`,
    );
    return;
  }
  await vscode.env.clipboard.writeText(ticket!);
  void vscode.window.showInformationMessage(
    'cc-connect: Room started. Ticket copied to clipboard.',
  );
  roomsProvider?.refresh();
  openRoomPanelForTopic(context, topic!);
}

async function joinRoom(context: vscode.ExtensionContext): Promise<void> {
  const ticket = await vscode.window.showInputBox({
    prompt: 'Paste a cc-connect Ticket',
    placeHolder: 'cc1-…',
    ignoreFocusOut: true,
    validateInput: (v) =>
      v.trim().startsWith('cc1-') ? undefined : 'Ticket must start with cc1-',
  });
  if (!ticket) return;

  let topic: string;
  try {
    topic = await vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: 'cc-connect: joining Room…',
        cancellable: false,
      },
      () => startChatDaemon(ticket.trim()),
    );
  } catch (e) {
    void vscode.window.showErrorMessage(
      `cc-connect: failed to join Room — ${(e as Error).message}`,
    );
    return;
  }
  roomsProvider?.refresh();
  openRoomPanelForTopic(context, topic);
}

function openRoomPanelForTopic(
  context: vscode.ExtensionContext,
  topic: string,
): void {
  const distRoot = vscode.Uri.joinPath(
    context.extensionUri,
    'dist',
    'webview',
  );

  const panel = vscode.window.createWebviewPanel(
    'cc-connect.room',
    `cc-connect — ${topic.slice(0, 8)}`,
    vscode.ViewColumn.One,
    {
      enableScripts: true,
      retainContextWhenHidden: true,
      localResourceRoots: [distRoot],
    },
  );

  panel.webview.html = getRoomHtml(panel.webview, distRoot);

  panel.webview.onDidReceiveMessage(
    async (msg: { type?: string; body?: unknown }) => {
      if (msg.type === 'chat:send') {
        const body = typeof msg.body === 'string' ? msg.body.trim() : '';
        if (!body) return;
        // Slash-command parse. v0 supports `/drop <path>`. Anything
        // else passes through as a chat message.
        const dropMatch = /^\/drop\s+(.+)$/.exec(body);
        const resp = dropMatch
          ? await ccDrop(topic, dropMatch[1].trim())
          : await ccSend(topic, body);
        if (!resp.ok) {
          panel.webview.postMessage({
            type: 'chat:send-error',
            body: resp.err ?? 'unknown ipc error',
          });
        }
      }
    },
    undefined,
    context.subscriptions,
  );

  const myNick = readMyNick() ?? '(me)';
  panel.webview.postMessage({
    type: 'room:state',
    body: { topic, myNick },
  });

  const runner = createClaudeRunner({
    topic,
    onEvent: (event) =>
      panel.webview.postMessage({ type: 'claude:event', body: event }),
    onStateChange: (state) =>
      panel.webview.postMessage({ type: 'claude:state', body: state }),
  });

  let tail: LogTailHandle | undefined;
  try {
    tail = tailLog(topic, (m: Message) => {
      panel.webview.postMessage({ type: 'chat:message', body: m });
      // Wake Claude on @-mentions. Skip ONLY messages authored by our
      // own AI mirror (`<myNick>-cc`) — otherwise Claude's own outputs
      // would re-trigger on themselves and loop. The user's own
      // messages still fire so self-instruction (typing
      // `@<myNick>-cc do X` or `@me ...` in your own chat) works
      // per design D1.
      const fromOwnAi = !!myNick && m.nick === `${myNick}-cc`;
      if (!fromOwnAi && myNick && shouldWakeClaude(m.body, myNick)) {
        runner.enqueue(m.body);
      }
    });
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    vscode.window.showErrorMessage(`cc-connect: log tail failed — ${msg}`);
  }

  panel.onDidDispose(
    () => {
      tail?.close();
      runner.abort();
    },
    undefined,
    context.subscriptions,
  );

  panel.webview.postMessage({ type: 'host:ready' });
}

async function pickTopic(): Promise<string | undefined> {
  const roomsDir = path.join(os.homedir(), '.cc-connect', 'rooms');
  let entries: string[];
  try {
    entries = fs.readdirSync(roomsDir).filter((n) => {
      try {
        return fs.statSync(path.join(roomsDir, n)).isDirectory();
      } catch {
        return false;
      }
    });
  } catch {
    void vscode.window.showErrorMessage(
      'cc-connect: ~/.cc-connect/rooms/ not found. Start a Room with `cc-connect: Start Room`.',
    );
    return undefined;
  }
  if (entries.length === 0) {
    void vscode.window.showErrorMessage(
      'cc-connect: no Rooms found. Start one with `cc-connect: Start Room`.',
    );
    return undefined;
  }

  const items = entries.map<vscode.QuickPickItem & { topic: string }>(
    (topic) => ({
      label: `$(comment-discussion) ${topic.slice(0, 12)}…`,
      description: topic,
      topic,
    }),
  );
  const picked = await vscode.window.showQuickPick(items, {
    placeHolder: 'Select a Room (topic hex)',
    matchOnDescription: true,
  });
  return picked?.topic;
}

async function stopRoom(topicArg?: string): Promise<void> {
  const topic = topicArg ?? (await pickTopic());
  if (!topic) return;
  const confirm = await vscode.window.showWarningMessage(
    `Stop daemons for ${topic.slice(0, 12)}…? Peers will lose connection.`,
    { modal: true },
    'Stop',
  );
  if (confirm !== 'Stop') return;
  try {
    await vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: `cc-connect: stopping ${topic.slice(0, 12)}…`,
        cancellable: false,
      },
      async () => {
        // Stop chat-daemon first (closes chat.sock for clients), then
        // host-bg (drops the gossip subscription). Order is best-effort
        // — if either errors we still try the other.
        const errs: string[] = [];
        try {
          await stopChatDaemon(topic);
        } catch (e) {
          errs.push(`chat-daemon: ${(e as Error).message}`);
        }
        try {
          await stopHostBg(topic);
        } catch (e) {
          errs.push(`host-bg: ${(e as Error).message}`);
        }
        if (errs.length > 0) throw new Error(errs.join('; '));
      },
    );
  } catch (e) {
    void vscode.window.showErrorMessage(
      `cc-connect: stop failed — ${(e as Error).message}`,
    );
    return;
  }
  void vscode.window.showInformationMessage(
    `cc-connect: ${topic.slice(0, 12)}… stopped.`,
  );
  roomsProvider?.refresh();
}

async function showTicket(topicArg?: string): Promise<void> {
  const topic = topicArg ?? (await pickTopic());
  if (!topic) return;
  const ticket = readTicketForTopic(topic);
  if (!ticket) {
    void vscode.window.showErrorMessage(
      `cc-connect: no ticket recorded for ${topic.slice(0, 12)}… ` +
        '(daemon not running, or PID file missing).',
    );
    return;
  }
  await vscode.env.clipboard.writeText(ticket);
  void vscode.window.showInformationMessage(
    `cc-connect: ticket for ${topic.slice(0, 12)}… copied to clipboard.`,
  );
}

function readTicketForTopic(topic: string): string | undefined {
  const pidPath = path.join(
    os.homedir(),
    '.cc-connect',
    'rooms',
    topic,
    'chat-daemon.pid',
  );
  try {
    const raw = fs.readFileSync(pidPath, 'utf8');
    const parsed = JSON.parse(raw) as { ticket?: string };
    return typeof parsed.ticket === 'string' ? parsed.ticket : undefined;
  } catch {
    return undefined;
  }
}

function readMyNick(): string | undefined {
  try {
    const configPath = path.join(os.homedir(), '.cc-connect', 'config.json');
    const raw = fs.readFileSync(configPath, 'utf8');
    const cfg = JSON.parse(raw) as { self_nick?: string };
    return cfg.self_nick;
  } catch {
    return undefined;
  }
}

function getRoomHtml(webview: vscode.Webview, distRoot: vscode.Uri): string {
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
  <title>cc-connect — Room</title>
  <style>
    body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif; padding: 16px; color: var(--vscode-foreground); background: var(--vscode-editor-background); }
    h1 { font-size: 14px; margin: 0 0 4px; font-weight: 600; }
    .room-meta { font-size: 11px; opacity: 0.5; margin: 0 0 12px; font-family: var(--vscode-editor-font-family, monospace); }
    h2 { margin: 0 0 8px; font-size: 13px; opacity: 0.7; font-weight: 500; }
    .panes { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
    .pane { border: 1px solid var(--vscode-panel-border); border-radius: 6px; padding: 12px; min-height: 240px; }
    .muted { font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; opacity: 0.6; }
    .chat-log { font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; line-height: 1.55; max-height: 60vh; overflow-y: auto; }
    .chat-line { display: grid; grid-template-columns: 60px 80px 1fr; gap: 8px; padding: 2px 0; align-items: baseline; }
    .chat-line .ts { opacity: 0.4; font-variant-numeric: tabular-nums; }
    .chat-line .nick { font-weight: 600; opacity: 0.85; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .chat-line.me .nick { color: var(--vscode-textLink-foreground); }
    .chat-line .body { opacity: 0.95; word-wrap: break-word; }
    .mention { font-weight: 500; color: var(--vscode-textLink-foreground); }
    .mention.me { background: var(--vscode-editor-selectionHighlightBackground, rgba(255,200,0,0.18)); padding: 0 3px; border-radius: 2px; }
    .mention.broadcast { color: var(--vscode-symbolIcon-eventForeground, var(--vscode-textLink-foreground)); font-style: italic; }
    .chat-input { margin-top: 8px; }
    .chat-input textarea { width: 100%; box-sizing: border-box; resize: vertical; min-height: 36px; max-height: 200px; padding: 6px 8px; font: inherit; font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; line-height: 1.4; color: var(--vscode-input-foreground); background: var(--vscode-input-background); border: 1px solid var(--vscode-input-border, transparent); border-radius: 3px; outline: none; }
    .chat-input textarea:focus { border-color: var(--vscode-focusBorder); }
    .claude-log { font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; line-height: 1.55; max-height: 60vh; overflow-y: auto; }
    .claude-row { padding: 2px 0; word-wrap: break-word; }
    .claude-system { opacity: 0.5; }
    .claude-hook { opacity: 0.4; padding-left: 12px; }
    .claude-text { white-space: pre-wrap; opacity: 0.95; }
    .claude-tool { color: var(--vscode-textLink-foreground); }
    .claude-result { opacity: 0.6; padding-top: 4px; border-top: 1px dashed var(--vscode-panel-border); margin-top: 4px; }
    .claude-error { color: var(--vscode-errorForeground); }
    .claude-other { opacity: 0.35; }
    .claude-tool-card { margin: 6px 0; padding: 6px 8px; border-left: 2px solid var(--vscode-textLink-foreground); background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.08)); border-radius: 0 3px 3px 0; }
    .claude-tool-card.claude-tool-error { border-left-color: var(--vscode-errorForeground); }
    .claude-tool-head { display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }
    .claude-tool-name { font-weight: 600; color: var(--vscode-textLink-foreground); }
    .claude-tool-input { opacity: 0.85; word-break: break-all; }
    .claude-tool-result { margin-top: 4px; padding-left: 12px; opacity: 0.75; white-space: pre-wrap; word-wrap: break-word; }
    .claude-tool-pending { opacity: 0.5; margin-left: auto; }
    .claude-tool-empty { margin-top: 4px; opacity: 0.45; font-style: italic; }
    .tool-body { margin-top: 6px; }
    .tool-body-pre { font-family: var(--vscode-editor-font-family, monospace); font-size: 11.5px; line-height: 1.45; padding: 6px 8px; background: var(--vscode-textCodeBlock-background, rgba(127,127,127,0.10)); border-radius: 3px; overflow-x: auto; max-height: 300px; overflow-y: auto; white-space: pre; margin: 0; }
    .tool-body.tool-body-error .tool-body-pre { background: var(--vscode-inputValidation-errorBackground, rgba(255,80,80,0.10)); border: 1px solid var(--vscode-errorForeground, rgba(255,80,80,0.5)); }
    .tool-expand { margin-top: 4px; padding: 1px 8px; font-size: 11px; opacity: 0.7; background: transparent; color: var(--vscode-textLink-foreground); border: 1px solid var(--vscode-panel-border); border-radius: 2px; cursor: pointer; }
    .tool-expand:hover { opacity: 1; }
    .diff { margin-top: 6px; display: flex; flex-direction: column; gap: 4px; font-family: var(--vscode-editor-font-family, monospace); font-size: 11.5px; line-height: 1.45; }
    .diff-old, .diff-new { margin: 0; padding: 4px 8px; border-radius: 3px; overflow-x: auto; max-height: 240px; overflow-y: auto; white-space: pre; }
    .diff-old { background: var(--vscode-diffEditor-removedTextBackground, rgba(255,80,80,0.12)); border-left: 2px solid var(--vscode-gitDecoration-deletedResourceForeground, #d04444); }
    .diff-new { background: var(--vscode-diffEditor-insertedTextBackground, rgba(80,200,80,0.12)); border-left: 2px solid var(--vscode-gitDecoration-addedResourceForeground, #44d044); }
    .md { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif; font-size: 13px; line-height: 1.5; }
    .md > *:first-child { margin-top: 0; }
    .md > *:last-child { margin-bottom: 0; }
    .md p { margin: 4px 0; }
    .md h1, .md h2, .md h3, .md h4, .md h5, .md h6 { margin: 12px 0 4px; line-height: 1.3; font-weight: 600; }
    .md h1 { font-size: 1.3em; }
    .md h2 { font-size: 1.18em; }
    .md h3 { font-size: 1.08em; }
    .md h4, .md h5, .md h6 { font-size: 1em; }
    .md ul, .md ol { margin: 4px 0; padding-left: 24px; }
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
    .pane-busy { opacity: 0.7; font-weight: 400; font-size: 11px; }
    button { font: inherit; padding: 4px 10px; background: var(--vscode-button-background); color: var(--vscode-button-foreground); border: none; border-radius: 3px; cursor: pointer; }
    button:hover { background: var(--vscode-button-hoverBackground); }
    .actions { margin-top: 16px; }
    .status { opacity: 0.7; font-size: 12px; }
  </style>
</head>
<body>
  <div id="root"></div>
  <script type="module" src="${scriptUri}"></script>
</body>
</html>`;
}
