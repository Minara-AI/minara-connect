import * as crypto from 'crypto';
import * as vscode from 'vscode';

export function activate(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('cc-connect.hello', () => {
      vscode.window.showInformationMessage('cc-connect: hello');
    }),
    vscode.commands.registerCommand('cc-connect.openRoom', () => {
      openRoomPanel(context);
    }),
  );
}

export function deactivate(): void {}

function openRoomPanel(context: vscode.ExtensionContext): void {
  const panel = vscode.window.createWebviewPanel(
    'cc-connect.room',
    'cc-connect — Room',
    vscode.ViewColumn.One,
    {
      enableScripts: true,
      retainContextWhenHidden: true,
    },
  );

  panel.webview.html = getRoomHtml(panel.webview);

  panel.webview.onDidReceiveMessage(
    (msg: { type?: string; body?: unknown }) => {
      if (msg.type === 'echo:request') {
        vscode.window.showInformationMessage(
          `cc-connect: webview said "${String(msg.body)}"`,
        );
        panel.webview.postMessage({ type: 'echo:reply', body: 'pong' });
      }
    },
    undefined,
    context.subscriptions,
  );

  panel.webview.postMessage({ type: 'host:ready' });
}

function getRoomHtml(webview: vscode.Webview): string {
  const nonce = crypto.randomBytes(16).toString('base64');
  const csp = [
    "default-src 'none'",
    `script-src 'nonce-${nonce}'`,
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
    h1 { font-size: 14px; margin: 0 0 12px; }
    .panes { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
    .pane { border: 1px solid var(--vscode-panel-border); border-radius: 6px; padding: 12px; min-height: 240px; }
    .pane h2 { margin: 0 0 8px; font-size: 13px; opacity: 0.7; font-weight: 500; }
    .log { font-family: var(--vscode-editor-font-family, monospace); font-size: 12px; white-space: pre-wrap; }
    button { font: inherit; padding: 4px 10px; background: var(--vscode-button-background); color: var(--vscode-button-foreground); border: none; border-radius: 3px; cursor: pointer; }
    button:hover { background: var(--vscode-button-hoverBackground); }
    #status { opacity: 0.7; font-size: 12px; margin-top: 16px; }
  </style>
</head>
<body>
  <h1>cc-connect — placeholder</h1>
  <div class="panes">
    <div class="pane">
      <h2>chat</h2>
      <div class="log" id="chat-log">(no messages — Step 2 will wire chat-ui)</div>
    </div>
    <div class="pane">
      <h2>claude</h2>
      <div class="log" id="claude-log">(no Claude session — Step 4 will wire SDK)</div>
    </div>
  </div>
  <p style="margin-top: 16px;">
    <button id="echo-btn">Echo to host</button>
  </p>
  <p id="status">waiting for host…</p>
  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();
    const status = document.getElementById('status');
    document.getElementById('echo-btn').addEventListener('click', () => {
      vscode.postMessage({ type: 'echo:request', body: 'ping at ' + new Date().toISOString() });
    });
    window.addEventListener('message', (event) => {
      const msg = event.data || {};
      if (msg.type === 'host:ready') {
        status.textContent = 'host ready ✓';
      } else if (msg.type === 'echo:reply') {
        status.textContent = 'host replied: ' + String(msg.body);
      }
    });
  </script>
</body>
</html>`;
}
