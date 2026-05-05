import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as vscode from 'vscode';
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

export function activate(context: vscode.ExtensionContext): void {
  roomsProvider = new RoomsProvider();
  roomPanelProvider = new RoomPanelProvider(context);

  context.subscriptions.push(
    vscode.window.registerTreeDataProvider('cc-connect.rooms', roomsProvider),
    vscode.window.registerWebviewViewProvider(
      RoomPanelProvider.viewType,
      roomPanelProvider,
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
  await openRoomInPanel(topic!);
}

async function joinRoom(): Promise<void> {
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
