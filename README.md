# cc-connect

A peer-to-peer protocol that lets multiple Claude Code instances share the same chat-and-files context. Each developer keeps their own Claude. The shared substrate (chat history, files) lives over P2P (`iroh-gossip`); each Claude reads from its local replica via a `UserPromptSubmit` hook.

The big idea: don't multiplex one Claude across humans, multiplex shared context across Claudes.

> v0.1 status: feature-complete in commits, full protocol drafted in [`PROTOCOL.md`](./PROTOCOL.md). Vendored ed25519 patches block crates.io publish until upstream releases an `ed25519-dalek` against fixed `pkcs8` (see [`TODOS.md`](./TODOS.md)).

> ⚠ **Read [`SECURITY.md`](./SECURITY.md) before inviting anyone to a Room.** A Ticket is a capability — anyone holding it can read your chat, drop files, and prompt-inject your Claude. v0.1 has no end-to-end Message signatures and no Ticket revocation. The threat model lays out exactly what is and isn't protected.

---

## How the magic moment works

```
┌──────── Alice's machine ────────┐         ┌──────── Bob's machine ────────┐
│                                  │         │                                │
│  $ cc-connect room start         │         │  $ cc-connect room join cc1-…  │
│   ┌── claude ──┐  ┌── chat ──┐   │ gossip  │   ┌── claude ──┐ ┌── chat ──┐  │
│   │            │  │           │  │ ──────► │   │            │ │           │ │
│   └────────────┘  └───────────┘  │ ◄────── │   └────────────┘ └───────────┘ │
│                                  │         │                                │
│  Alice asks her Claude:          │         │  Bob types in his chat pane:   │
│  "Redis or Postgres?"            │         │  "postgres, we have it"        │
│                                  │         │                                │
│  Hook fires on Alice's next      │         │                                │
│  prompt → injects Bob's message  │         │                                │
│  into Alice's Claude context.    │         │                                │
│  Alice's Claude: "going Postgres │         │                                │
│  per the chat"                   │         │                                │
└──────────────────────────────────┘         └────────────────────────────────┘
```

Bob never typed anything special. Alice never copy-pasted anything. The hook reads Bob's messages from a locally-replicated `log.jsonl` and prepends them to Alice's prompt.

Full architecture: [`PROTOCOL.md`](./PROTOCOL.md). Decision rationale: [`docs/adr/`](./docs/adr/).

---

## Install

You need: macOS or Linux, a working Claude Code install. **Rust is not required** for the default path — the bootstrap downloads a pre-built binary for your platform.

### One-liner (recommended — no Rust needed)

```bash
curl -fsSL https://raw.githubusercontent.com/Minara-AI/cc-connect/main/scripts/bootstrap.sh | bash
```

Detects your platform (macOS arm64 / x86_64, Linux x86_64), pulls the matching tarball from the latest GitHub release, verifies its sha256, then runs the bundled `install.sh --skip-build` to register the `UserPromptSubmit` hook + `cc-connect-mcp` server in `~/.claude/`, symlink binaries into `~/.local/bin/`, and run `cc-connect doctor`. Total time: ~30 seconds on a fast network. Idempotent — safe to re-run.

Pin a specific version (handy for CI):

```bash
curl -fsSL <…/bootstrap.sh> | CC_CONNECT_VERSION=v0.1.0 bash
```

### Build from source (developers / unsupported platforms)

If you want to hack on cc-connect, or your platform isn't in the release matrix (e.g. Linux aarch64, BSD), build from source — needs Rust ≥ 1.89:

```bash
# One-liner, source mode:
curl -fsSL <…/bootstrap.sh> | CC_CONNECT_FROM_SOURCE=1 bash

# Or clone + install yourself:
git clone https://github.com/Minara-AI/cc-connect.git
cd cc-connect
./install.sh
```

`install.sh` checks the toolchain (offers `rustup` if Rust is missing), builds the workspace, backs up `~/.claude/settings.json`, idempotently registers the hook + MCP server, symlinks every binary, runs `cc-connect doctor`. `--yes` for unattended, `--skip-build` to reuse an existing `target/release/`. First build takes ~5–10 minutes (iroh stack + vendored ed25519).

**Restart Claude Code afterwards** (either path) so it picks up the new hook + MCP tools. After install, every command is available as `cc-connect …` from any directory.

### Build the VSCode extension

The default way to use cc-connect is in your editor — see [next section](#use-it-in-vscode-recommended).

```bash
cd vscode-extension
bun install
bun run compile
bunx @vscode/vsce package
code --install-extension cc-connect-vscode-0.1.0.vsix
```

Or, for development: open `vscode-extension/` in VSCode and press `F5` to launch an Extension Development Host. Once the extension is published to the GitHub release, you'll be able to skip the build step entirely and download the `.vsix` directly.

---

## Use it in VSCode (recommended)

The cleanest day-to-day experience is the editor extension. Both halves of cc-connect — the chat substrate and your Claude Code session — live inside one VSCode panel, no terminal multiplexer needed.

```
┌─ Activity Bar ────────────────────────────────────────────┐
│  cc-connect            Rooms                              │
│  ▸ team-A   ALIVE                                         │
│  ▸ design   ALIVE                                         │
│  ▸ debug    DORMANT                                       │
└───────────────────────────────────────────────────────────┘
┌─ Bottom panel ─────────────────────────────────────────────┐
│  team-A…   @alice   ready          [📋 copy ticket]        │
├─ [💬 Chat]  [✦ Claude  3] ─────────────────────────────────┤
│  ┌─ chat ─────────────────────┐  ┌─ claude ─────────────┐ │
│  │  @bob: postgres, we have it │  │ ○ Thought for 2s     │ │
│  │  (me): yes, on it           │  │ ● cc_send · 13 bytes │ │
│  │                              │  │ ● cc_wait_for_…      │ │
│  │ [Message · @ to mention]    │  │ [Ask Claude…    ]🛡️→│ │
│  └──────────────────────────────┘  └──────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

Drag the Room panel to the **secondary side bar** for a vertical Slack-style split next to your editor.

### Quick start

1. Click the cc-connect activity-bar icon (left edge). If `~/.local/bin/cc-connect` isn't installed, the Rooms view's welcome message points you to a setup walkthrough — follow it (or run the bootstrap one-liner above) before continuing.
2. Click **Start Room** in the Rooms tree title bar (or **Join Room** with a peer's ticket).
3. The Room panel opens; Claude auto-greets the room and starts listening for `@you-cc` mentions.
4. Click **copy ticket** at the top of the Room panel to share with a peer.

### What's in the Room panel

| Tab | What it gives you |
|---|---|
| **Chat** | IM-style rows: own messages right-aligned with iMessage bubbles, peers on the left. `@`-mention autocomplete from recent senders. `/` button opens a slash-command picker (`/drop`, `/at`). `+` button opens VSCode's native file picker → drops the file into the room. |
| **Claude** | Your local Claude Code session for this Room. Tool calls render as IN/OUT cards with VSCode-native styling + per-tool codicons. Live "Thought for Xs" indicator. Full-markdown text replies. Active-editor chip above the input → click to attach `@<workspace-relative-path>` to your prompt. |

### Permission modes (Claude pane bottom-right pill)

Click the pill to cycle:

| Mode | Behaviour |
|---|---|
| **auto** (default) | Every tool runs without asking. The cc-connect Room model is "trusted substrate"; this is the ergonomic default. |
| **ask edits** | Claude can read freely; `Edit` / `Write` / `Bash` calls prompt for approval. |
| **plan** | Claude can read but cannot run any side-effectful tool. |
| **ask all** | Every tool call shows an inline **Allow / Deny / Always allow** bubble in the Claude log. The textarea greys out until you decide. |

### Other niceties

- **Conversation history** — the clock-icon button in the Claude pane lists every past Claude session for this workspace (parses `~/.claude/projects/`); click one to replay it read-only.
- **Auto-greet on join** — uses the same `bootstrap-prompt.md` + `auto-reply-prompt.md` as the TUI launcher, so the embedded Claude knows it's in a Room and enters the listener loop without you typing anything.
- **File-reference chips** — paths the user types in the Claude prompt render as clickable codicons; click to open the file in the editor.
- **New chat** — `+` icon in the Claude pane head mints a fresh `sessionId` without closing the Room.
- **Tickets are interchangeable** — Tickets minted by the VSCode extension are byte-identical to those from `cc-connect room start`. Use either side freely.

The extension is **purely TypeScript** — no native code, no extra runtime deps beyond the cc-connect binaries you already installed. Source: [`vscode-extension/`](./vscode-extension).

---

## Or via the terminal (TUI alternative)

The TUI experience is unchanged — same Room model, same Tickets, same hook injection. Pick whichever you prefer; mix freely between machines. Two commands cover everything:

```bash
# Start a brand-new room. Spawns a background host daemon, opens the TUI.
cc-connect room start

# Join an existing room by ticket. Same TUI experience.
cc-connect room join cc1-…
```

That's it. Everything else (the host daemon, the chat substrate, the MCP server) is started for you and torn down when you `Ctrl-Q` (host daemons stay alive in the background so peers can still join via your ticket — close them with `cc-connect clear`).

### What `room start` shows you

```
┌─ cc-connect [1-9] tab [Ctrl-N] new [Ctrl-W] close [F2/Tab] pane [Ctrl-Y] copy ─┐
│ [1] team-A·H   [2] design                                                       │  ← tab strip
├────────────────────────────────────────────────────────────────────────────────┤
│ ┌─ 🤖 claude · team-A ───────────────┐ ┌─ 💬 chat · team-A ─────────────────┐ │
│ │ $                                  │ │ [bob] use postgres                  │ │
│ │                                    │ │ (@me) [alice] @dave PR ?            │ │
│ │                                    │ │ › yes, on it                        │ │
│ └────────────────────────────────────┘ └────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

| Key            | Action |
|---             |---     |
| `1`–`9`        | Switch to tab N |
| `Ctrl-N`       | Open new tab → `j` to paste a ticket and join |
| `Ctrl-W`       | Close active tab. If you started the host daemon for it, prompts whether to also stop the daemon |
| `F2` / `Tab`   | Switch focus between chat and claude panes |
| `Ctrl-Y`       | Copy the active tab's ticket to your system clipboard |
| `PgUp/PgDn`    | Scroll the focused pane (or use trackpad / mouse wheel) |
| `Ctrl-Q`       | Quit (closes all tabs; keeps host daemons alive) |

The `·H` suffix on a tab label means you started a `host-bg` daemon for that room. Close the tab without stopping the daemon and the room stays joinable for your peers.

### Optional: configure your displayed name

```bash
cc-connect room start --nick alice          # persists to ~/.cc-connect/config.json
```

Or skip the flag and the first run will prompt you for one.

### Optional: prefer a multiplexer

The TUI is the default, but if you have `zellij` or `tmux` installed you can opt in to a multiplexer-managed layout (left pane: claude, right pane: a richer Bun + React + Ink chat panel):

```bash
CC_CONNECT_MULTIPLEXER=zellij cc-connect room start
CC_CONNECT_MULTIPLEXER=tmux   cc-connect room start
CC_CONNECT_MULTIPLEXER=auto   cc-connect room start   # zellij → tmux → embedded TUI
```

Same exit hint: `Ctrl-q + y` (zellij), `Ctrl-b + d` (tmux detach), or `Ctrl-Q` (embedded TUI).

### Optional: self-hosted relay

By default cc-connect routes through n0's free public relay cluster (used by every iroh deployment). To run through your own server:

```bash
cc-connect room start --relay https://relay.yourdomain.com
```

The host's `--relay` URL is baked into the printed ticket, so joiners pick it up automatically — they only need to pass `--relay` themselves to override. Stand-up instructions: [`.claude/skills/cc-connect-relay-setup/SKILL.md`](.claude/skills/cc-connect-relay-setup/SKILL.md).

---

## Two-laptop demo procedure

For the real magic-moment test:

1. Both machines: install (above), then restart Claude Code.
2. Alice: `cc-connect room start` — copy the printed `cc1-…` ticket.
3. Bob: `cc-connect room join 'cc1-…'`.
4. **Bob types into his chat pane**: `try sqlite for now`.
5. **Alice asks her Claude something** in the left pane (anything). On submit, the hook reads Bob's message from Alice's local log and injects it as context. Alice's Claude reply should reference Bob's suggestion.

If it doesn't work, see [Troubleshooting](#troubleshooting).

---

## Sharing files

Inside the chat pane:

```
> /drop ./design.svg
[chat] dropped design.svg (148 bytes)
```

`/drop <path>` hashes the file into a local `iroh-blobs` `MemStore`, broadcasts a tiny gossip Message announcing the hash, then peers fetch the bytes out-of-band over the iroh-blobs ALPN against your NodeId. Both peers' Claudes see it as `@file:<path>` on the next prompt.

**v0.2 cap: 1 GiB per file.** Bytes flow via iroh-blobs, not gossip. Files persist for the lifetime of the room's chat-daemon. The `cc_drop` MCP tool refuses sensitive paths by default (SSH/AWS/GPG/Kube/Docker credentials, `.env*`, `id_rsa*`, `*.pem`, etc.); override per-process with `CC_CONNECT_DROP_ALLOW_DANGEROUS=1`. See [`SECURITY.md`](./SECURITY.md).

---

## Letting Claude talk back (MCP tools)

The TUI starts the `cc-connect-mcp` server the first time you run it. The embedded Claude gets seven tools:

| Tool                      | What it does |
|---                        |---           |
| `cc_send`                 | Broadcast a chat message into your room |
| `cc_at`                   | Same as `cc_send`, but with `@<nick>` prefix |
| `cc_drop`                 | Share a local file with peers (iroh-blobs) |
| `cc_recent`               | Last N chat lines from this room's log |
| `cc_list_files`           | Files dropped into the room (with local paths) |
| `cc_save_summary`         | Overwrite this room's rolling summary (auto-injected on every prompt) |
| `cc_wait_for_mention`     | Block until someone @-mentions this Claude (or a timeout) |

Try it: in a TUI claude pane, ask "send '@all standup in 5' to the room". Claude calls `cc_at` and the message lands in everyone's chat scrollback.

---

## Layered context injection

Every prompt's hook output is composed from three sections, each budget-bounded to keep the total ≤ 8 KiB (PROTOCOL §7.3 step 6 / ADR-0004):

```
[cc-connect summary]                            ← rolling summary (≤ 1.5 KiB)
  Discussed Postgres vs SQLite (decided Postgres). …

[cc-connect files]                              ← INDEX.md tail (≤ 1.5 KiB)
  - bob    design.svg  (148B)  @file:/Users/.../files/01XX-design.svg
  - alice  api.md      (4096B) @file:/Users/.../files/01YY-api.md

[chatroom @bob 12:00Z] use postgres             ← unread chat verbatim (~5 KiB)
[chatroom for-you @alice 12:01Z] @dave PR ?
```

`INDEX.md` is auto-maintained — every file_drop appends a line. `summary.md` is Claude-driven: ask the embedded Claude to "summarise the room and save it" and it'll call `cc_save_summary`.

---

## Command reference

`cc-connect room start` and `cc-connect room join` are the only commands you need day-to-day. Everything below is supporting / management / debug surface — most of it is invoked for you by the room launcher.

| Command | Audience | What it does |
|---      |---       |---           |
| `cc-connect room start` | **everyone** | Mint a fresh ticket, spawn the host-bg daemon, open the TUI. The recommended entry point. |
| `cc-connect room join <ticket>` | **everyone** | Join an existing room by ticket, open the TUI. The recommended entry point. |
| `cc-connect doctor` | everyone | Sanity-check the install. Prints binary mtimes, hook entry, MCP entry, identity perms. Run this if anything's misbehaving. |
| `cc-connect clear` | everyone | Stop every running cc-connect background process (chat-daemons + host-bg). Use if a daemon got stuck or before reinstalling a fresh build. `--purge` also wipes `~/.cc-connect/rooms/`. |
| `cc-connect upgrade` | everyone | `git pull` + rebuild + reinstall in one shot. Identity + nicknames are preserved. `--yes` skips the y/N. |
| `cc-connect uninstall` | everyone | Reverse `install.sh` entirely: stop daemons, strip the hook + MCP entries, remove `~/.local/bin` symlinks. `--purge` also wipes `~/.cc-connect/`, `/tmp/cc-connect-$UID/`, and stale `~/.claude/*.json.bak.*` backups. |
| `cc-connect host-bg list` | management | List running background-host daemons (one line per daemon). |
| `cc-connect host-bg stop <topic-prefix>` | management | SIGTERM a specific daemon by topic-hex prefix. |
| `cc-connect host-bg start [--relay <url>]` | management | Start a daemon without opening the TUI. Mainly for headless / CI scenarios. `room start` does this for you. |
| `cc-connect chat-daemon {list,stop,start}` | management | Same shape as `host-bg`, but for chat-session daemons (the gossip + chat.sock side; only matters in the multiplexer path). |
| `cc-connect host` | low-level | Bare-bones blocking host (no TUI, no claude, no MCP). Mostly useful for protocol smoke tests. Prefer `room start`. |
| `cc-connect chat <ticket>` | low-level | Bare-bones REPL-only joiner (no TUI). Mostly useful for protocol smoke tests. Prefer `room join`. |
| `cc-connect host-bg-daemon` | internal | Daemon entry point. Don't run directly — `host-bg start` spawns it. |
| `cc-connect chat-daemon-daemon` | internal | Same shape, chat-daemon side. Don't run directly. |

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `cc-connect room start` hangs at "binding endpoint" | Firewall blocks n0's relay servers | Try a different network. |
| Joiner sees `(joined late, no history available)` | Both peers already moved past pre-join messages, or backfill RPC failed | Re-test on a clean room; if persistent, run with `CC_CONNECT_GOSSIP_DEBUG=1` and inspect `~/.cc-connect/gossip-debug.log`. |
| Room says `(peers: 1)` but no messages flow | mDNS is blocked (corporate WiFi client isolation) | Try a coffee-shop / home network. |
| Hook silently does nothing | Settings.json hook path is relative, or stale binary on PATH | `cc-connect doctor` — it prints the registered hook path + binary mtimes. `cc-connect upgrade` to refresh. |
| Restarted Claude Code but it still doesn't see chat | Old `cc-connect-mcp` child still running | `cc-connect clear`, then restart Claude Code. |
| Can't see remote peer's messages but they see yours | Stale daemon from before the post-Apr fixes | `cc-connect clear` on both machines, `cc-connect upgrade`, retry. |
| `cargo build` fails on `ed25519-3.0.0-rc.4` | Missing `[patch.crates-io]` (you cloned without `vendored/`) | Re-clone or `git fetch origin main && git reset --hard origin/main`. |
| Identity file mode wrong | Drifted from `0600` | `chmod 600 ~/.cc-connect/identity.key`. The loader and doctor both warn. |
| `/tmp/cc-connect-$UID/` mode wrong / pre-existed as a symlink | Hostile co-tenant or earlier crash | `rm -rf "$TMPDIR/cc-connect-$UID/" && cc-connect room start`. PROTOCOL §8 mandates a 0700 non-symlink parent. |

If `cc-connect-hook` fired but you suspect it failed, check `~/.cc-connect/hook.log`. The hook always exits 0 (PROTOCOL §7.4) so errors don't propagate to Claude Code.

---

## Project layout

```
cc-connect/
├── PROTOCOL.md              v0.1 wire-and-disk specification
├── CONTEXT.md               Domain glossary (DDD-style)
├── SECURITY.md              Threat model
├── CLAUDE.md                Agent guide for Claude Code sessions in this repo
├── docs/
│   ├── adr/                 Architecture decision records
│   └── agents/              Per-repo config the engineering skills consume
├── crates/
│   ├── cc-connect-core/     Protocol primitives library (104 tests)
│   ├── cc-connect/          host / chat / room / host-bg / chat-daemon / lifecycle / doctor binary
│   ├── cc-connect-tui/      Embedded TUI binary + library
│   ├── cc-connect-mcp/      MCP stdio server (Claude Code → chat tools)
│   └── cc-connect-hook/     UserPromptSubmit hook binary
├── chat-ui/                 Bun + React + Ink chat panel (→ cc-chat-ui), used in zellij/tmux paths
├── layouts/                 zellij KDL + tmux script + claude-wrap.sh + prompt files
├── .claude/skills/          Project-local Claude Code skills
├── .githooks/               Polyglot pre-commit + commit-msg hooks
├── scripts/                 install / smoke-test / repo-config helpers
├── tests/                   FAKE-CLAUDE-CODE integration test
└── vendored/                Patched ed25519 + ed25519-dalek (temporary)
```

---

## Status / contributing

Want to contribute? Read [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the dev setup, commit conventions, and PR checklist. The [`CONTEXT.md`](./CONTEXT.md) glossary is load-bearing — domain terms in the codebase must match it. Architectural decisions get an [ADR](./docs/adr/); wire-format changes get a `v` bump per [`PROTOCOL.md`](./PROTOCOL.md).

Bugs and feature requests: [GitHub Issues](https://github.com/Minara-AI/cc-connect/issues/new/choose). Security: [private advisory](https://github.com/Minara-AI/cc-connect/security/advisories/new), not a public issue ([`SECURITY.md`](./SECURITY.md)).

## Release process

### Release tag namespaces

cc-connect ships **two independent artifacts** with their own release cadence. The namespace lives in the tag, not in separate repos — pick the right tag pattern for what you're releasing:

| Artifact | Tag pattern | What it ships | CI workflow |
|---|---|---|---|
| **cc-connect CLI / TUI** (Rust binaries) | `v0.1.0`, `v0.2.0-rc.1` | `cc-connect`, `cc-connect-hook`, `cc-chat-ui` tarballs per platform attached to the GitHub release | [`release.yml`](./.github/workflows/release.yml) |
| **VSCode extension** | `vscode-extension-v0.1.0`, `vscode-extension-v0.2.0-rc.1` | `cc-connect-vscode-<version>.vsix` attached to the GitHub release | [`vscode-extension-release.yml`](./.github/workflows/vscode-extension-release.yml) |

The two pipelines are completely independent — bumping one never triggers the other. The version numbers don't have to track each other either; the extension declares the minimum cc-connect binary it needs through `package.json` (and the [VSCode usage section](#use-it-in-vscode-recommended) makes the dependency explicit for users). Cutting a release:

```bash
# CLI / TUI
git tag v0.2.0
git push origin v0.2.0          # → release.yml builds tarballs

# VSCode extension (bump vscode-extension/package.json::version first —
# the workflow refuses to build if the tag and package.json disagree)
$EDITOR vscode-extension/package.json   # version: "0.1.0" → "0.2.0"
git add vscode-extension/package.json
git commit -m "chore(vscode-extension): bump to 0.2.0"
git tag vscode-extension-v0.2.0
git push origin main vscode-extension-v0.2.0   # → vscode-extension-release.yml packages .vsix
```

The extension workflow refuses to build if the tag version doesn't match `vscode-extension/package.json::version` — keeps the on-disk version, the tag, and the .vsix filename in lockstep.

### Install / uninstall surface contract

`cc-connect uninstall` and `cc-connect upgrade` are user-facing promises: a clean wipe and a clean reinstall. Honoring those promises is a release-time discipline.

**Every release MUST keep the cleanup surface in sync with the install surface.** The cleanup lives in [`crates/cc-connect/src/lifecycle.rs`](./crates/cc-connect/src/lifecycle.rs); when a release adds anything to the install surface — a new binary, a new `~/.claude/settings.json` key, a new file under `~/.cc-connect/`, a new MCP tool that registers itself somewhere — the matching removal must land in `lifecycle.rs` in the same PR.

For every release-shaped PR (anything touching `install.sh`, `crates/cc-connect/src/setup.rs`, or persistent file paths), the reviewer checks:

- Did `INSTALLED_BIN_NAMES` get the new binary?
- Did `run_clear` get the new daemon's `run_stop`?
- Did `remove_hook_from_settings` / `remove_mcp_from_claude_json` get the new JSON key?
- Did `--purge` (or another explicit removal step) cover any new persistent file outside `~/.cc-connect/`?

The contract is: a user who runs `cc-connect uninstall --purge` ends up with **zero** cc-connect-touched state on their machine, regardless of which version installed it.

## License

Dual-licensed under [MIT](./LICENSE-MIT) **OR** [Apache-2.0](./LICENSE-APACHE) at your option. Contributions are accepted under the same dual license; there is no separate CLA. Participants in project spaces are expected to follow the [Code of Conduct](./CODE_OF_CONDUCT.md).
