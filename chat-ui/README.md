# cc-chat-ui

The chat panel of cc-connect. Bun + React + Ink. Lives next to `claude` in
a multiplexer pane (zellij or tmux); the multiplexer handles window
layout so this app only renders chat scrollback + an input box.

## What it does

- Tails `~/.cc-connect/rooms/<topic>/log.jsonl` for the message stream.
- Talks to the chat-daemon's IPC socket
  (`~/.cc-connect/rooms/<topic>/chat.sock`) for sends.
- Renders left/right bubble layout (own messages right, peer messages
  left), per-message timestamps, and `@me` red highlights for peer
  @-mentions of you.
- @-mention completion popup driven by recently-seen peer nicks.

## What it does NOT do

- Embed Claude's PTY (the multiplexer's other pane runs `claude` itself).
- Manage rooms (one process per room; multiplexer windows handle multi-room).
- Decode tickets (chat-daemon does it; chat-ui reads the ticket from the
  daemon's PID file purely to display + copy).
- Replace `cc-connect-tui` (kept as a no-multiplexer fallback).

## Dev

```sh
bun install
bun run dev -- --topic <topic_hex>
```

## Build the standalone binary

```sh
bun run build      # → ../target/release/cc-chat-ui
```

`install.sh` runs this automatically as part of the workspace build.

## Hotkeys

| Key             | Action                                      |
| --------------- | ------------------------------------------- |
| `Ctrl-Q`        | quit                                        |
| `Ctrl-Y`        | copy ticket to clipboard                    |
| `PgUp` / `PgDn` | scrollback                                  |
| `Tab` / `Enter` | accept @-mention completion (popup visible) |
| `Esc`           | dismiss completion popup                    |
| `Enter`         | send (popup hidden)                         |
