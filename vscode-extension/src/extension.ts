import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as vscode from 'vscode';
import {
  checkCcConnectBinary,
  type BinaryHealth,
} from './host/binaryVersion';
import {
  NICK_MAX_BYTES,
  readSelfNick,
  selfNickConfigured,
  validateNick,
  writeSelfNick,
} from './host/config';
import {
  startChatDaemon,
  startHostBg,
  stopChatDaemon,
  stopHostBg,
} from './host/daemon';
import { RoomPanelProvider } from './panel/RoomPanelProvider';
import { RoomsProvider } from './sidebar/RoomsProvider';

let roomsProvider: RoomsProvider | undefined;
let roomPanelProvider: RoomPanelProvider | undefined;

async function refreshSetupContext(): Promise<BinaryHealth> {
  const health = await checkCcConnectBinary();
  void vscode.commands.executeCommand(
    'setContext',
    'cc-connect.setupRequired',
    !health.ok,
  );
  return health;
}

async function ensureSetup(): Promise<boolean> {
  const health = await refreshSetupContext();
  if (health.ok) return true;
  // Surface a toast tailored to the failure mode. The Rooms tree's
  // viewsWelcome already shows the static fallback; this is the
  // mid-flow nudge when the user clicks Start/Join Room while
  // unhealthy.
  let message: string;
  switch (health.reason) {
    case 'missing':
      message =
        'cc-connect binary not found at ~/.local/bin/cc-connect. Run setup first.';
      break;
    case 'outdated':
      message = `cc-connect binary is ${health.version}, but this extension needs ${health.required} or newer. Run \`cc-connect upgrade\` (or re-run the bootstrap installer).`;
      break;
    case 'unreadable':
      message = `Could not read cc-connect version (${health.detail}). The binary may be broken; try reinstalling.`;
      break;
  }
  const action = await vscode.window.showWarningMessage(
    message,
    'Open setup guide',
    'I just upgraded',
  );
  if (action === 'Open setup guide') {
    await vscode.commands.executeCommand('cc-connect.openSetup');
  } else if (action === 'I just upgraded') {
    await refreshSetupContext();
    roomsProvider?.refresh();
  }
  return false;
}

export function activate(context: vscode.ExtensionContext): void {
  roomsProvider = new RoomsProvider();
  roomPanelProvider = new RoomPanelProvider(context);

  // Drive the Rooms view's welcome state — `viewsWelcome` (in
  // package.json) toggles between "needs setup" and "no rooms yet"
  // markdown based on this context key. Fire-and-forget: the welcome
  // view defaults to "needs setup" until the probe completes a moment
  // later, which is fine — first-render delay is < 100ms.
  void refreshSetupContext();

  context.subscriptions.push(
    vscode.window.registerTreeDataProvider('cc-connect.rooms', roomsProvider),
    vscode.window.registerWebviewViewProvider(
      RoomPanelProvider.viewType,
      roomPanelProvider,
      // Keep the webview alive when another sidebar/panel view takes
      // focus. Without this, VSCode disposes the webview on hide:
      // React state is lost, the runner is aborted, and the auto-greet
      // re-broadcasts to peers when the panel comes back. Memory cost
      // is one chat scrollback per Room, well under the budget.
      { webviewOptions: { retainContextWhenHidden: true } },
    ),
    vscode.commands.registerCommand('cc-connect.hello', () => {
      vscode.window.showInformationMessage('cc-connect: hello');
    }),
    vscode.commands.registerCommand(
      'cc-connect.openRoom',
      (arg?: string | { topic: string }) => {
        const topic = resolveTopicArg(arg);
        void openRoom(topic);
      },
    ),
    vscode.commands.registerCommand('cc-connect.startRoom', () => {
      void startRoom();
    }),
    vscode.commands.registerCommand('cc-connect.joinRoom', () => {
      void joinRoom();
    }),
    vscode.commands.registerCommand('cc-connect.setNickname', () => {
      void setNickname();
    }),
    vscode.commands.registerCommand(
      'cc-connect.deleteRoom',
      (arg?: string | { topic: string }) => {
        void deleteRoom(resolveTopicArg(arg));
      },
    ),
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
      void refreshSetupContext();
      roomsProvider?.refresh();
    }),
    vscode.commands.registerCommand('cc-connect.openSetup', () => {
      void vscode.commands.executeCommand(
        'workbench.action.openWalkthrough',
        'minara.cc-connect-vscode#cc-connect.setup',
        false,
      );
    }),
    vscode.commands.registerCommand('cc-connect.copyBootstrapCommand', () => {
      const cmd =
        'curl -fsSL https://raw.githubusercontent.com/Minara-AI/cc-connect/main/scripts/bootstrap.sh | bash';
      void vscode.env.clipboard.writeText(cmd);
      void vscode.window.showInformationMessage(
        'cc-connect: bootstrap command copied. Paste it into a terminal.',
      );
    }),
  );
}

export function deactivate(): void {}

function resolveTopicArg(
  arg: string | { topic: string } | undefined,
): string | undefined {
  if (typeof arg === 'string') return arg;
  if (arg && typeof arg === 'object' && typeof arg.topic === 'string') {
    return arg.topic;
  }
  return undefined;
}

async function openRoom(topic: string | undefined): Promise<void> {
  const t = topic ?? (await pickTopic());
  if (!t) return;
  await openRoomInPanel(t);
}

/** Reveal the bottom-panel cc-connect view, then point it at `topic`.
 *  The first arg to `view.focus` doesn't exist as a public API — the
 *  view's auto-generated focus command (`<viewId>.focus`) does. */
async function openRoomInPanel(topic: string): Promise<void> {
  // Set the room first so resolveWebviewView (called by VSCode if the
  // view hasn't been resolved yet) sees the topic on activation.
  roomPanelProvider?.setRoom(topic);
  await vscode.commands.executeCommand(`${RoomPanelProvider.viewType}.focus`);
}

async function startRoom(): Promise<void> {
  if (!(await ensureSetup())) return;
  if (!(await ensureSelfNick())) return;
  let ticket: string | undefined;
  let topic: string | undefined;
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
  if (!ticket || !topic) return; // unreachable — withProgress only resolves on success — but appeases TS
  await vscode.env.clipboard.writeText(ticket);
  void vscode.window.showInformationMessage(
    'cc-connect: Room started. Ticket copied to clipboard.',
  );
  roomsProvider?.refresh();
  await openRoomInPanel(topic);
}

async function joinRoom(): Promise<void> {
  if (!(await ensureSetup())) return;
  if (!(await ensureSelfNick())) return;
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
  await openRoomInPanel(topic);
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

/** Prompt for `self_nick` if it has never been recorded in
 *  ~/.cc-connect/config.json. Returns false only if the user dismissed
 *  the prompt — which we treat as "abort the room start" so they aren't
 *  silently registered as `<pubkey-prefix>-cc` to peers. An empty
 *  answer is allowed and persists as "" so we don't ask again. */
async function ensureSelfNick(): Promise<boolean> {
  if (selfNickConfigured()) return true;
  const suggested = (() => {
    try {
      return os.userInfo().username || '';
    } catch {
      return '';
    }
  })();
  const nick = await vscode.window.showInputBox({
    title: 'cc-connect: pick a display name',
    prompt:
      'Other peers see this as your sender label. Leave blank to use a short pubkey prefix.',
    placeHolder: 'e.g. alice',
    value: suggested,
    ignoreFocusOut: true,
    validateInput: (v) => validateNick(v),
  });
  if (nick === undefined) return false;
  try {
    writeSelfNick(nick);
  } catch (e) {
    void vscode.window.showErrorMessage(
      `cc-connect: ${(e as Error).message}`,
    );
    return false;
  }
  return true;
}

async function setNickname(): Promise<void> {
  const current = readSelfNick() ?? '';
  const nick = await vscode.window.showInputBox({
    title: 'cc-connect: set your display name',
    prompt: `Other peers see this as your sender label (max ${NICK_MAX_BYTES} bytes). Leave blank to use a short pubkey prefix.`,
    value: current,
    ignoreFocusOut: true,
    validateInput: (v) => validateNick(v),
  });
  if (nick === undefined) return;
  try {
    writeSelfNick(nick);
  } catch (e) {
    void vscode.window.showErrorMessage(
      `cc-connect: ${(e as Error).message}`,
    );
    return;
  }
  const trimmed = nick.trim();
  void vscode.window.showInformationMessage(
    trimmed === ''
      ? 'cc-connect: nickname cleared. Peers will see your pubkey prefix. Restart any open Rooms to apply.'
      : `cc-connect: nickname set to "${trimmed}". Restart any open Rooms to apply.`,
  );
}

async function deleteRoom(topicArg?: string): Promise<void> {
  const topic = topicArg ?? (await pickTopic());
  if (!topic) return;
  const roomDir = path.join(os.homedir(), '.cc-connect', 'rooms', topic);
  // Belt-and-braces: even though the menu only exposes Delete on
  // `room.dormant` items, double-check the chat-daemon really is gone
  // before nuking the history.
  const pidPath = path.join(roomDir, 'chat-daemon.pid');
  try {
    const raw = fs.readFileSync(pidPath, 'utf8');
    const parsed = JSON.parse(raw) as { pid?: number };
    if (typeof parsed.pid === 'number') {
      try {
        process.kill(parsed.pid, 0);
        void vscode.window.showWarningMessage(
          `cc-connect: ${topic.slice(0, 12)}… still has a running chat-daemon. Stop it first via Stop Room…`,
        );
        return;
      } catch {
        // ESRCH — daemon is gone, the .pid file is stale. Safe to delete.
      }
    }
  } catch {
    // No PID file = dormant. Proceed.
  }
  const confirm = await vscode.window.showWarningMessage(
    `Delete chat history for ${topic.slice(0, 12)}…? This removes the local replica (log.jsonl, summary, dropped files) and cannot be undone.`,
    { modal: true },
    'Delete',
  );
  if (confirm !== 'Delete') return;
  try {
    fs.rmSync(roomDir, { recursive: true, force: true });
  } catch (e) {
    void vscode.window.showErrorMessage(
      `cc-connect: delete failed — ${(e as Error).message}`,
    );
    return;
  }
  // Best-effort: sweep a stale host-bg PID file if one was left behind.
  // (host-bg PID files live in a sibling dir, not under rooms/<topic>/.)
  const hostPid = path.join(
    os.homedir(),
    '.cc-connect',
    'hosts',
    `${topic}.pid`,
  );
  try {
    fs.unlinkSync(hostPid);
  } catch {
    // Missing or in-use — fine, list_running() sweeps stale ones anyway.
  }
  void vscode.window.showInformationMessage(
    `cc-connect: ${topic.slice(0, 12)}… deleted.`,
  );
  roomsProvider?.refresh();
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
