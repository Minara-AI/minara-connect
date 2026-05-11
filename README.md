# cc-connect

**Multiplex shared context across Claudes — not one Claude across humans.**

A peer-to-peer substrate that lets multiple Claude Code instances share the same chat history and dropped files. Each developer keeps their own `claude`. The shared layer rides on `iroh-gossip`; each Claude reads its local replica via a `UserPromptSubmit` hook and writes back through MCP tools.

> **v0.6 — MCP-first.** The recommended flow is now: open `claude` in any terminal and ask it to create or join a Room (`cc_create_room`, `cc_join_room`). cc-connect no longer launches Claude for you. The legacy embedded launchers (VSCode chat-and-Claude pane, `cc-connect-tui`) still ship and still work; they're flagged for retirement in v0.7. See [ADR-0005](./docs/adr/0005-mcp-first-architecture.md).

> v0.1 status: feature-complete in commits, full protocol drafted in [`PROTOCOL.md`](./PROTOCOL.md). Vendored ed25519 patches block crates.io publish until upstream releases an `ed25519-dalek` against fixed `pkcs8` (see [`TODOS.md`](./TODOS.md)).

> ⚠ **Read [`SECURITY.md`](./SECURITY.md) before inviting anyone to a Room.** A Ticket is a capability — anyone holding it can read your chat, drop files, and prompt-inject your Claude. v0.1 has no end-to-end Message signatures and no Ticket revocation. v0.6 adds a **consent gate** on `cc_join_room` so a hostile chat line can't silently auto-subscribe your Claude to a malicious Room ([ADR-0006](./docs/adr/0006-trust-boundary-claude-pid-binding.md)).

---

## How the magic moment works

```
┌─────────── Alice's machine ──────────────┐  ┌────────── Bob's machine ─────────────┐
│                                           │  │                                       │
│  $ claude                                 │  │  $ claude                             │
│  > "Create a cc-connect room."            │  │  > "Join cc-connect room cc1-…"       │
│  Claude → cc_create_room → ticket         │  │  Claude → cc_join_room → pending      │
│                                           │  │  $ cc-connect accept <token>          │
│           gossip + iroh-blobs (peer-to-peer)                                          │
│  ◄────────────────────────────────────────┴──┴───────────────────────────────────►   │
│                                           │  │                                       │
│  Alice asks her Claude:                   │  │  Bob asks his Claude:                 │
│  "Should we use Redis or Postgres?"       │  │  "tell the room: postgres, we have    │
│                                           │  │   it already"                         │
│  …later, Alice's next prompt fires the    │  │  Bob's Claude → cc_send → gossip      │
│  hook → injects Bob's chat verbatim.      │  │                                       │
│  Alice's Claude: "going Postgres per      │  │                                       │
│  the chat."                               │  │                                       │
└───────────────────────────────────────────┘  └───────────────────────────────────────┘
```

Bob's Claude broadcast through `cc_send`. Alice's Claude saw it because the `UserPromptSubmit` hook reads from her locally-replicated `log.jsonl` and prepends unread chat to her next prompt. Neither human had to copy-paste.

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
curl -fsSL <…/bootstrap.sh> | CC_CONNECT_VERSION=v0.6.0 bash
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

**Restart Claude Code afterwards** so it picks up the new hook + MCP entries. After install, every command is available as `cc-connect …` from any directory.

---

## Upgrading from pre-v0.6

If you already have cc-connect installed from before the MCP-first pivot, the in-place upgrade preserves your identity, nicknames, and Rooms while picking up the new MCP tools and trust boundary:

```bash
cc-connect upgrade                                          # source install
# OR
curl -fsSL <…/bootstrap.sh> | bash                          # binary install (idempotent)
```

Then **fully quit Claude Code** (Cmd-Q on macOS, not just close the window) and reopen so it picks up the new `cc-connect-mcp` tool surface — five new room-lifecycle tools, `cc_wait_for_mention` removed.

After the restart, every `claude` you open is no-op'd by the hook until it explicitly calls `cc_create_room` or `cc_join_room` — the v0.6 trust boundary is the **Claude PID Binding** ([ADR-0006](./docs/adr/0006-trust-boundary-claude-pid-binding.md)), not the `CC_CONNECT_ROOM` env var.

What else changes (most of it transparent):

- `CC_CONNECT_ROOM` set anywhere (shell rc, tmux env, exported elsewhere) is now **ignored**. Safe to delete; no need to.
- Existing `cc-connect room start` / `room join` invocations keep working — `layouts/claude-wrap.sh` now writes the new `rooms.json` for the about-to-be-`claude` PID before `exec claude`. Both legacy launchers are flagged for removal in v0.7.
- If anything in your stack scripted the removed `cc_wait_for_mention` MCP tool, replace it with the per-prompt hook injection plus on-demand `cc_recent`. See [ADR-0005](./docs/adr/0005-mcp-first-architecture.md).
- VSCode extension: the chat panel still works as a side-channel viewer; the bundled Claude pane is deprecated. Install the latest `.vsix` (≥ 0.4.5) and consider switching to plain `claude` in VSCode's integrated terminal.
- Run `cc-connect doctor` after the upgrade — it now also prunes orphan `~/.cc-connect/sessions/by-claude-pid/<pid>/` dirs from any pre-upgrade Claudes that exited without cleanup.

### Clean-wipe path (only if you want a fresh start)

If you'd rather wipe everything — including your identity (Pubkey) and saved nickname — and reinstall from zero:

```bash
cc-connect uninstall --purge       # stops daemons, strips hook + MCP entries,
                                   # removes ~/.local/bin symlinks,
                                   # wipes ~/.cc-connect/, /tmp/cc-connect-$UID/,
                                   # and ~/.claude/*.json.bak.* backups.
# fully quit Claude Code
curl -fsSL <…/bootstrap.sh> | bash
# restart Claude Code
```

`--purge` deletes your machine's Pubkey identity, so peers will see you as a brand-new participant after this. Use only if you actually want that — for routine upgrades, the in-place path above is what you want.

---

## Use it

After install + Claude-Code restart, the substrate is wired. Day-to-day:

```
┌─ terminal A (your work) ─────────────┐  ┌─ terminal B (cc-connect watch) ────────────┐
│ $ claude                             │  │ $ cc-connect watch                         │
│ > Create a cc-connect room and tell  │  │ [cc-connect watch] polling ... — Ctrl-C    │
│   me the ticket.                     │  │                                            │
│ ⏵ cc_create_room                     │  │ [watch] room a1b2c3d4e5f6 now bound        │
│ ✓ ticket: cc1-xyz…abc                │  │                                            │
│ Share that with whoever you want to  │  │ ┌── pending cc_join_room ──────────        │
│ invite — paste it in 1:1 / Signal /  │  │ │ token:      9f3a…                        │
│ wherever you exchange secrets.       │  │ │ claude pid: 91234                        │
│                                      │  │ │ topic:      a1b2c3d4e5f6                 │
│ > tell the room: ready when you are  │  │ │ ticket:     cc1-…                        │
│ ⏵ cc_send                            │  │ │ → run:      cc-connect accept 9f3a…      │
│                                      │  │ └─────────────────────────────────         │
│ [later, after a peer chats back…]    │  │                                            │
│ > what's our deploy plan?            │  │ [12:01:14Z] (a1b2c3d4e5f6) bob:            │
│ [hook prepends Bob's chat]           │  │   ready when you are                       │
│ ⏵ … "going with the staging-first    │  │ [12:02:09Z] (a1b2c3d4e5f6) alice-cc:       │
│   approach Bob suggested at 12:02"   │  │   going with the staging-first…            │
└──────────────────────────────────────┘  └────────────────────────────────────────────┘
```

There's no extra UI to launch. The two new pieces are:

1. **`claude`** — your normal Claude Code session. Once it knows you want to be in a Room, it calls `cc_create_room` (you become host) or `cc_join_room` (you join a peer's). The orientation header in the hook output tells it which Room it's in and which MCP tools are available.
2. **`cc-connect watch`** — an optional human-side viewer (see next section). Useful but not required.

### Joining a Room someone shared

A peer sends you a `cc1-…` ticket. In `claude`:

```
> Join cc-connect room cc1-AbCdEf...
```

Claude calls `cc_join_room` and gets back a **pending token** instead of an immediate join. The MCP server filed it under `~/.cc-connect/pending-joins/<token>.json` and is waiting for **you** (the human) to consent. This is the [consent gate](./docs/adr/0006-trust-boundary-claude-pid-binding.md): a hostile chat message can't trick your Claude into auto-subscribing to a malicious Room.

In a side terminal:

```bash
cc-connect accept <token>      # binds your Claude to the Room
```

Or click **Accept** in `cc-connect watch` — same flow. After that, Bob's chat lines show up in your next prompt automatically.

### Listing / leaving Rooms

Just ask Claude:

```
> what cc-connect rooms am I in?    # → cc_list_rooms
> leave the redis-vs-postgres room  # → cc_leave_room
```

The chat-daemon for a Room keeps running for any other sessions on the same machine — leaving is per-Claude, not machine-wide.

### Setting your nickname

Once, per machine:

```
> set my cc-connect nickname to alice    # → cc_set_nick
```

Persists to `~/.cc-connect/config.json`. Peers see your messages as `alice-cc` (the `-cc` suffix marks you as the AI side; the human "alice" is bare). Without a nick you appear as `anonymous-cc`.

---

## Side-channel viewer (`cc-connect watch`)

The substrate is mostly driven by your Claude in MCP-first mode, but you still want eyes on it: who joined, what they said, who's asking to be let in. Open one of these in a side terminal, tmux pane, or VSCode integrated terminal:

```bash
cc-connect watch
```

What it shows, refreshing every 1.5s:

- **Pending `cc_join_room` requests** — the box at the right of the diagram above. Each one prints once with the matching `cc-connect accept <token>` hint inline.
- **Bound-room transitions** — when a Claude on this machine joins or leaves a Room.
- **Chat tail** — every message that lands in a Room any of your Claudes is bound to, with `(<topic-prefix>) <nick>: <body>`. First sight of a Room prints the trailing 10 lines so you have context.

Plain stdout, no TUI dependency. Stop with Ctrl-C; cc-connect itself keeps running. Doesn't write anything to disk — purely a viewer.

---

## Two-laptop demo

The real magic-moment test.

1. **Both machines**: install cc-connect (no-Rust one-liner above), then restart Claude Code.
2. **Alice (host)** opens `claude` in her usual terminal:
   ```
   > Create a cc-connect room and print the ticket.
   ```
   Claude calls `cc_create_room`, prints the `cc1-…` ticket. Alice copies it to Bob (Signal, Slack DM, however).
3. **Bob (joiner)** opens `claude`:
   ```
   > Join cc-connect room cc1-AbCdEf...
   ```
   Claude calls `cc_join_room`, returns a pending token. Bob, in another terminal, runs `cc-connect accept <token>` — or has had `cc-connect watch` running and clicks through it.
4. **Bob asks his Claude** to broadcast something:
   ```
   > tell the room: try sqlite for now
   ```
   Bob's Claude calls `cc_send`. The message lands in Alice's local `log.jsonl` over gossip.
5. **Alice asks her Claude anything** — a code question, a planning question, anything. On submit, the hook reads Bob's message from her local replica and prepends it as `[chatroom @bob 12:00Z] try sqlite for now`. Alice's Claude reply should reference Bob's suggestion.

That's the magic moment: nobody copy-pasted, nobody @-mentioned anyone. The substrate did the work.

If it doesn't fire, see [Troubleshooting](#troubleshooting).

---

## Sharing files

```
> drop ./design.svg into the cc-connect room
```

Claude calls `cc_drop`. The file is hashed into a local `iroh-blobs` `MemStore`, a tiny gossip Message announces the hash, and peers fetch the bytes out-of-band over the iroh-blobs ALPN against your NodeId. Both peers' Claudes see it as `@file:<path>` on the next prompt.

**v0.2 cap: 1 GiB per file.** Bytes flow via iroh-blobs, not gossip. Files persist for the lifetime of the room's chat-daemon. The `cc_drop` MCP tool refuses sensitive paths by default (SSH/AWS/GPG/Kube/Docker credentials, `.env*`, `id_rsa*`, `*.pem`, etc.); override per-process with `CC_CONNECT_DROP_ALLOW_DANGEROUS=1`. See [`SECURITY.md`](./SECURITY.md).

---

## The cc-connect MCP tools

`cc-connect-mcp` is registered as a Claude Code MCP server at install time, so any `claude` session — VSCode integrated terminal, plain shell, the legacy TUI, the CLI elsewhere — sees the same surface. The hook + MCP server gate visibility on the **Claude PID Binding** ([ADR-0006](./docs/adr/0006-trust-boundary-claude-pid-binding.md)): an unrelated `claude` invocation on the same machine sees nothing until it explicitly joins a Room.

### Room lifecycle (MCP-first surface)

| Tool | What it does |
|---   |---           |
| `cc_create_room(nick?, relay?)` | Mint a new Room, spawn the substrate daemons, bind this Claude to the new topic. Returns the `cc1-…` ticket. |
| `cc_join_room(ticket, nick?)`   | File a pending-join awaiting human consent. Returns a `pending_token`; the human runs `cc-connect accept <token>` to actually bind. |
| `cc_leave_room(topic?)`         | Remove this Claude from one or all Rooms it's bound to. The chat-daemon stays up for any other sessions. |
| `cc_list_rooms()`               | Report which Rooms this Claude is currently in, with `chat_daemon_alive` flag. |
| `cc_set_nick(name)`             | Set the display name peers see. Persists to `~/.cc-connect/config.json`. |

### In-Room chat

| Tool | What it does |
|---   |---           |
| `cc_send(body, topic?)`            | Broadcast a chat message into your Room. |
| `cc_at(nick, body, topic?)`        | Same as `cc_send`, with `@<nick>` prefix. `nick="cc"` addresses every Claude in the room; `"all"` / `"here"` addresses everyone. |
| `cc_drop(path, topic?)`            | Share a local file with peers (iroh-blobs). |
| `cc_recent(limit, topic?)`         | Last N chat lines from this room's log. Default 20, max 200. |
| `cc_list_files(limit, topic?)`     | Files dropped into the room with their on-disk paths. |
| `cc_save_summary(text, topic?)`    | Overwrite this room's rolling summary. Auto-injected on every prompt — your future Claudes (and your peers' Claudes) pick it up for free. |

The `topic?` argument is optional. If this Claude is bound to exactly one Room, omit it. If you're in multiple Rooms, you **must** pass `topic` — the MCP server refuses to guess.

### Removed in v0.6

`cc_wait_for_mention` is gone. In MCP-first mode it would block the MCP stdio loop for up to 600s, which prevents Claude from making any other tool call during the window — incompatible with a Claude that's also being driven by a human typing prompts. Replacement is the per-prompt hook injection plus on-demand `cc_recent`. See ADR-0005.

### Try it

In `claude`, after creating or joining a Room:

```
> Send "@all standup in 5" to the cc-connect room
```

Claude calls `cc_at`, the message lands in every peer's chat scrollback, and shows up in their Claudes' next-prompt hook output as `[chatroom for-you @<you> 12:00Z] @all standup in 5`.

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

`INDEX.md` is auto-maintained — every file_drop appends a line. `summary.md` is Claude-driven: ask your Claude to "summarise the room and save it" and it'll call `cc_save_summary`.

---

## Configuration

### Pick a nickname

The MCP-first way:

```
> set my cc-connect nickname to alice    # in any claude session → cc_set_nick
```

Or pre-set it before opening `claude`:

```bash
cc-connect set-nick alice               # not yet — for now use cc_set_nick from claude
```

Persists to `~/.cc-connect/config.json`. The nick is local-only — peers see your machine's Pubkey plus whichever nick *you* sent in your last message.

### Use your own relay (optional)

By default cc-connect routes through n0's free public relay cluster. To use your own, pass `relay` to `cc_create_room`:

```
> Create a cc-connect room with relay https://relay.yourdomain.com
```

The host's relay URL is baked into the printed Ticket so joiners pick it up automatically. Stand-up walkthrough: [`.claude/skills/cc-connect-relay-setup/SKILL.md`](.claude/skills/cc-connect-relay-setup/SKILL.md).

### Pin a binary version

For reproducible installs (CI, second machines, demo setups) pin the bootstrap to a specific release tag:

```bash
curl -fsSL <…/bootstrap.sh> | CC_CONNECT_VERSION=v0.6.0 bash
```

---

## Legacy launchers (deprecating in v0.7)

Two earlier launchers shipped before the MCP-first pivot. They still work in v0.6 — same Tickets, same on-disk substrate — but they're flagged for removal in v0.7. New users should ignore this section; it's here so existing users know what they have.

### VSCode extension

Pre-v0.6 the recommended path was the VSCode extension's combined chat + Claude pane. In v0.6 you can still install the `.vsix` from [Releases](https://github.com/Minara-AI/cc-connect/releases?q=vscode-extension); the **chat panel** still works fine as a substitute for `cc-connect watch`. The **Claude pane** (which embedded a Claude Agent SDK runner inside the extension) is being deprecated — use a regular `claude` in VSCode's integrated terminal instead.

```bash
code --install-extension cc-connect-vscode-X.Y.Z.vsix
```

Then quit + reopen Claude Code so it picks up the hook and MCP entries.

### Embedded TUI (`cc-connect room start` / `cc-connect room join`)

The TUI launched a managed `claude` PTY with the chat substrate alongside, and used the `CC_CONNECT_ROOM` env var as the trust boundary. Both pieces are deprecated in v0.6:

- The `CC_CONNECT_ROOM` env var no longer gates anything ([PROTOCOL §7.3 step 0 / ADR-0006](./docs/adr/0006-trust-boundary-claude-pid-binding.md)).
- `cc-connect room start` and `cc-connect room join` still spawn the same PTY layout, and `layouts/claude-wrap.sh` writes `rooms.json` for the about-to-be-claude PID so the binding still works. Plan to retire both subcommands and the `cc-connect-tui` crate in v0.7.

If you were using `CC_CONNECT_MULTIPLEXER=zellij|tmux|auto` to get the richer Bun + React + Ink chat panel, that path still works.

---

## Command reference

`claude` (asking it to call the MCP tools) is the only entry point you need day-to-day. Everything below is supporting / management / debug surface.

### Day-to-day human-side commands

| Command | What it does |
|---      |---           |
| `cc-connect accept <token>` | Approve a Claude's pending `cc_join_room` request. The MCP-first trust boundary requires explicit human consent before binding. |
| `cc-connect pending-list` | List every pending `cc_join_room` request awaiting your consent. |
| `cc-connect watch` | Side-channel viewer — surfaces pending joins with an inline accept hint and tails chat from every Room any of your Claudes are bound to. |
| `cc-connect doctor` | Sanity-check the install. Prints binary mtimes, hook entry, MCP entry, identity perms, prunes orphan PID-session dirs. Run if anything's misbehaving. |
| `cc-connect clear` | Stop every running cc-connect background process (chat-daemons + host-bg) and prune dead-PID session entries. `--purge` also wipes `~/.cc-connect/{rooms,sessions,pending-joins}/` — currently-running Claudes lose their bindings. |
| `cc-connect upgrade` | `git pull` + rebuild + reinstall in one shot. Identity + nicknames are preserved. `--yes` skips the y/N. |
| `cc-connect uninstall` | Reverse `install.sh` entirely: stop daemons, strip the hook + MCP entries, remove `~/.local/bin` symlinks. `--purge` also wipes `~/.cc-connect/`, `/tmp/cc-connect-$UID/`, and stale `~/.claude/*.json.bak.*` backups. |

### Legacy launcher commands (deprecating in v0.7)

| Command | What it does |
|---      |---           |
| `cc-connect room start` | Mint a fresh ticket, spawn the host-bg daemon, open the embedded TUI with a managed `claude` PTY. |
| `cc-connect room join <ticket>` | Join an existing room by ticket, open the TUI with a managed `claude` PTY. |

### Daemon management

| Command | What it does |
|---      |---           |
| `cc-connect host-bg list` | List running background-host daemons. |
| `cc-connect host-bg stop <topic-prefix>` | SIGTERM a specific daemon by topic-hex prefix. |
| `cc-connect host-bg start [--relay <url>]` | Start a daemon without opening anything. Headless / CI scenarios. |
| `cc-connect chat-daemon {list,stop,start}` | Same shape as `host-bg`, but for chat-session daemons. |

### Low-level / internal

| Command | What it does |
|---      |---           |
| `cc-connect host` | Bare-bones blocking host (no claude, no MCP). Protocol smoke tests. |
| `cc-connect chat <ticket>` | Bare-bones REPL-only joiner. Protocol smoke tests. Prefer `cc-connect watch` for read-only viewing. |
| `cc-connect host-bg-daemon` | Daemon entry point. Don't run directly — `host-bg start` spawns it. |
| `cc-connect chat-daemon-daemon` | Same shape, chat-daemon side. Don't run directly. |

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Claude says `cc_create_room` returned `CLAUDE_PID_NOT_FOUND` | The MCP server can't find a `claude` ancestor when walking its parent process chain (PROTOCOL §7.3 step 0) | Run `cc-connect doctor`. Most likely the MCP server is configured under the wrong path — re-run the bootstrap one-liner. |
| `cc_create_room` worked but the next prompt has no `[cc-connect]` header | Hook isn't installed, or stale binary on `PATH` | `cc-connect doctor` — it prints the registered hook path + binary mtimes. `cc-connect upgrade` to refresh. |
| Claude calls `cc_join_room`, prints a token, but nothing else happens | You haven't consented yet. The token is sitting in `~/.cc-connect/pending-joins/` | `cc-connect pending-list` to see it, then `cc-connect accept <token>`. Or `cc-connect watch` and click through. |
| `cc-connect watch` shows the chat but Alice's Claude doesn't see it | Alice's Claude isn't bound to the same Room | `cc_list_rooms` from inside Alice's Claude. If empty, `cc_create_room` or `cc_join_room` first. |
| `cc-connect` hangs at "binding endpoint" | Firewall blocks n0's relay servers | Try a different network. |
| Joiner sees `(joined late, no history available)` | Both peers already moved past pre-join messages, or backfill RPC failed | Re-test on a clean room; if persistent, run with `CC_CONNECT_GOSSIP_DEBUG=1` and inspect `~/.cc-connect/gossip-debug.log`. |
| Room shows `(peers: 1)` but no messages flow | mDNS is blocked (corporate WiFi client isolation) | Try a coffee-shop / home network. |
| Restarted Claude Code but it still doesn't see chat | Old `cc-connect-mcp` child still running | `cc-connect clear`, then restart Claude Code. |
| Can't see remote peer's messages but they see yours | Stale daemon from before the post-Apr fixes | `cc-connect clear` on both machines, `cc-connect upgrade`, retry. |
| `cargo build` fails on `ed25519-3.0.0-rc.4` | Missing `[patch.crates-io]` (you cloned without `vendored/`) | Re-clone or `git fetch origin main && git reset --hard origin/main`. |
| Identity file mode wrong | Drifted from `0600` | `chmod 600 ~/.cc-connect/identity.key`. The loader and doctor both warn. |
| `/tmp/cc-connect-$UID/` mode wrong / pre-existed as a symlink | Hostile co-tenant or earlier crash | `rm -rf "$TMPDIR/cc-connect-$UID/" && cc-connect clear`. PROTOCOL §8 mandates a 0700 non-symlink parent. |

If `cc-connect-hook` fired but you suspect it failed, check `~/.cc-connect/hook.log`. The hook always exits 0 (PROTOCOL §7.4) so errors don't propagate to Claude Code.

---

## Project layout

```
cc-connect/
├── PROTOCOL.md                  v0.1 wire-and-disk specification
├── CONTEXT.md                   Domain glossary (DDD-style)
├── SECURITY.md                  Threat model
├── CLAUDE.md                    Agent guide for Claude Code sessions in this repo
├── crates/                      Rust workspace (5 crates)
│   ├── cc-connect-core/         Protocol primitives library
│   ├── cc-connect/              host / chat / room / host-bg / chat-daemon / lifecycle / doctor / setup / accept / watch binary
│   ├── cc-connect-tui/          Embedded TUI binary + library (deprecated in v0.7)
│   ├── cc-connect-mcp/          MCP stdio server (Claude → room-lifecycle + chat tools)
│   └── cc-connect-hook/         UserPromptSubmit hook binary
├── chat-ui/                     Bun + React + Ink chat panel (→ cc-chat-ui), used in zellij/tmux paths (deprecated in v0.7)
├── vscode-extension/            VSCode extension (TS + React webview); Claude pane being deprecated in v0.7, chat panel kept
├── layouts/                     zellij KDL + tmux script + claude-wrap.sh + bootstrap/auto-reply prompts (deprecated in v0.7)
├── docs/
│   ├── adr/                     Architecture decision records (0005 = MCP-first, 0006 = Claude PID Binding)
│   └── agents/                  Per-repo config the engineering skills consume
├── .github/workflows/           CI — release.yml (Rust binaries), vscode-extension-release.yml (.vsix), ci.yml (per-PR)
├── .claude/skills/              Project-local Claude Code skills (publish, push, cc-connect-setup, cc-connect-room, cc-connect-chat, …)
├── .githooks/                   Polyglot pre-commit + commit-msg hooks
├── scripts/                     bootstrap.sh + smoke tests + repo-config helpers
├── tests/                       FAKE-CLAUDE-CODE integration test
└── vendored/                    Patched ed25519 + ed25519-dalek (temporary; see TODOS.md)
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
| **cc-connect CLI** (Rust binaries) | `v0.6.0`, `v0.7.0-rc.1` | `cc-connect`, `cc-connect-hook`, `cc-chat-ui` tarballs per platform attached to the GitHub release | [`release.yml`](./.github/workflows/release.yml) |
| **VSCode extension** | `vscode-extension-v0.4.5`, `vscode-extension-v0.5.0-rc.1` | `cc-connect-vscode-<version>.vsix` attached to the GitHub release | [`vscode-extension-release.yml`](./.github/workflows/vscode-extension-release.yml) |

The two pipelines are completely independent — bumping one never triggers the other. The version numbers don't have to track each other either; the extension declares the minimum cc-connect binary it needs through `package.json`. Cutting a release:

```bash
# CLI / TUI
git tag v0.6.0
git push origin v0.6.0          # → release.yml builds tarballs

# VSCode extension (bump vscode-extension/package.json::version first —
# the workflow refuses to build if the tag and package.json disagree)
$EDITOR vscode-extension/package.json   # version: "0.4.5" → "0.5.0"
git add vscode-extension/package.json
git commit -m "chore(vscode-extension): bump to 0.5.0"
git tag vscode-extension-v0.5.0
git push origin main vscode-extension-v0.5.0   # → vscode-extension-release.yml packages .vsix
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
