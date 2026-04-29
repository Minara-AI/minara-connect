#!/usr/bin/env bun
import React from "react";
import { render } from "ink";
import { App } from "./components/App.tsx";

interface ParsedArgs {
  topic: string | null;
  help: boolean;
}

function parseArgs(argv: readonly string[]): ParsedArgs {
  let topic: string | null = null;
  let help = false;
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "-h" || a === "--help") {
      help = true;
    } else if (a === "--topic") {
      topic = argv[i + 1] ?? null;
      i++;
    } else if (a !== undefined && a.startsWith("--topic=")) {
      topic = a.slice("--topic=".length);
    }
  }
  if (topic === null && process.env["CC_CONNECT_ROOM"]) {
    topic = process.env["CC_CONNECT_ROOM"];
  }
  return { topic, help };
}

function printHelp(): void {
  // eslint-disable-next-line no-console
  console.log(`cc-chat-ui — cc-connect chat panel (Bun + React + Ink)

Usage:
  cc-chat-ui --topic <topic_hex>
  CC_CONNECT_ROOM=<topic_hex> cc-chat-ui

Talks to the chat-daemon at:
  ~/.cc-connect/rooms/<topic>/chat.sock
  ~/.cc-connect/rooms/<topic>/log.jsonl

Hotkeys:
  Ctrl-Q     quit
  Ctrl-Y     copy ticket to clipboard
  PgUp/PgDn  scrollback
  Tab/Enter  accept @-mention completion
  Esc        dismiss completion popup
`);
}

function main(): void {
  const argv = process.argv.slice(2);
  const { topic, help } = parseArgs(argv);
  if (help) {
    printHelp();
    process.exit(0);
  }
  if (topic === null || topic.length === 0) {
    // eslint-disable-next-line no-console
    console.error(
      "cc-chat-ui: missing --topic <topic_hex> (or set CC_CONNECT_ROOM). Use -h for help.",
    );
    process.exit(2);
  }
  const { unmount } = render(<App topic={topic} />);
  // Render returns control; the React tree owns the loop until exit().
  process.on("SIGINT", () => {
    unmount();
    process.exit(0);
  });
}

main();
