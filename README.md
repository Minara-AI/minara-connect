# cc-connect

A peer-to-peer protocol that lets multiple Claude Code instances share the same chat-and-files context. Each developer keeps their own Claude. The shared substrate (chat history, files) lives over P2P (`iroh-gossip`); each Claude reads from its local replica via a `UserPromptSubmit` hook.

The big idea: don't multiplex one Claude across humans, multiplex shared context across Claudes.

> v0.1 status: feature-complete in commits, 76 tests passing, full protocol drafted in [`PROTOCOL.md`](./PROTOCOL.md). Vendored ed25519 patches block crates.io publish until upstream releases an `ed25519-dalek` against fixed `pkcs8` (see [`TODOS.md`](./TODOS.md)).

---

## How the magic moment works

```
┌──────── Alice's machine ────────┐         ┌──────── Bob's machine ────────┐
│                                  │         │                                │
│  tmux pane L:  $ claude          │         │  tmux pane L:  $ claude        │
│  tmux pane R:  $ cc-connect chat │ gossip  │  tmux pane R:  $ cc-connect    │
│                ── REPL ──        │ ──────► │                  chat <ticket> │
│                                  │ ◄────── │                                │
│  Alice asks her Claude:          │         │  Bob types in his chat REPL:   │
│  "Redis or Postgres?"            │         │  "postgres, we have it"        │
│                                  │         │                                │
│  Hook fires on Alice's next      │         │                                │
│  prompt → injects Bob's message  │         │                                │
│  into Alice's Claude context     │         │                                │
│  Alice's Claude: "going Postgres │         │                                │
│  per the chat"                   │         │                                │
└──────────────────────────────────┘         └────────────────────────────────┘
```

Bob never typed anything special. Alice never copy-pasted anything. The hook reads Bob's messages from a locally-replicated `log.jsonl` and prepends them to Alice's prompt.

Full architecture: [`PROTOCOL.md`](./PROTOCOL.md). Decision rationale: [`docs/adr/`](./docs/adr/).

---

## Setup (per machine)

You need: macOS or Linux, Rust ≥ 1.85 (or let the installer install it for you), a working Claude Code install.

### One-liner

```bash
git clone https://github.com/Minara-AI/cc-connect.git && cd cc-connect && ./install.sh
```

That's it. The script checks the toolchain (offers `rustup` if Rust is missing), runs the release build, backs up `~/.claude/settings.json` and merges the `UserPromptSubmit` hook entry idempotently, then runs `cc-connect doctor` to verify. Pass `--yes` for unattended, `--skip-build` to reuse an existing `target/release/`. Restart Claude Code afterwards so it picks up the new hook.

First build pulls the iroh stack and the patched-vendored `ed25519` / `ed25519-dalek` (see `vendored/`); takes ~5-10 minutes.

### Let Claude Code do it

Open Claude Code in any directory and paste:

> Clone https://github.com/Minara-AI/cc-connect, run its `install.sh`, then walk me through the `cc-connect doctor` output and tell me how to start a chat room.

The repo ships a `cc-connect-setup` skill at `.claude/skills/cc-connect-setup/SKILL.md`, so once Claude `cd`s into the clone it picks up the skill automatically and knows the failure modes.

### Manual install

If you'd rather not run the script, the equivalent steps:

1. `cargo build --workspace --release`.
2. Edit `~/.claude/settings.json` (merge with any existing `hooks` block):

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/cc-connect/target/release/cc-connect-hook"
          }
        ]
      }
    ]
  }
}
```

   Each entry under `UserPromptSubmit` is a `{matcher, hooks:[…]}` object — Claude Code's schema (an empty matcher matches every prompt). Use the **absolute path** — `cc-connect-hook` silently fails to inject if Claude Code's `PATH` doesn't include the binary's location.

3. `./target/release/cc-connect doctor` — should report `[OK]` for the hook entry, `[--]` (info: not yet created) for the identity key and active-rooms dir, and ideally no `[FAIL]` lines. Restart Claude Code after editing.

---

## Usage

### TUI mode (recommended)

One command, two panes — chat on the left, your Claude Code embedded on the right:

```bash
# Start a brand-new room (spawns a background host daemon, opens the TUI)
$ ./target/release/cc-connect room start

# Or join an existing room by ticket
$ ./target/release/cc-connect room join cc1-…
```

```
┌──────────┬─────────────────────────┐
│ chat     │  claude code            │
│  alice   │   $ ls                  │
│  > body  │   src/  README.md       │
│          │   $                     │
│ [type… ] │                         │
└──────────┴─────────────────────────┘
```

**Why this is nicer than running `host` and `chat` separately:**

- The Claude Code in the right pane only sees *this* room's chat — even if you have ten TUI windows open across ten projects, they don't cross-pollinate. Routing is by `CC_CONNECT_ROOM` env var read by the hook.
- `room start` spawns a `cc-connect host-bg` daemon that survives the TUI window. Close the TUI, the room stays joinable. Stop the daemon explicitly with `cc-connect host-bg stop <topic-prefix>` (or `cc-connect host-bg list` to see what's running).
- `Tab` switches focus between panes. `Ctrl-Q` quits. Standard Claude Code keybindings work in the right pane.

### Host a room (without the TUI)

```bash
$ ./target/release/cc-connect host

Room hosted. Share this code out-of-band:

    cc1-vxnqrtpgwvmjxd42zcnajikrl6dmbd4hamdj4twg…

Joiners run:  cc-connect chat <room-code>

Press Ctrl-C to close the room.
```

`host` stays online so joiners have a peer to dial. Share the `cc1-…` code via Slack / paper / whatever.

### Join a room

In a *separate* terminal pane:

```bash
$ ./target/release/cc-connect chat 'cc1-vxnqrtpgwvmjxd42zcnajikrl6dmbd4hamdj4twg…'

Joined room: a1b2c3d4e5f6 (peers: 1)
You are:     hnvcppgow2sc2yvd
[chatroom] (backfilled 7 messages from peer)
Type to send. Ctrl-C / EOF to leave.
```

Type messages. Press enter to send.

### Drop a file (v0.2)

```
> /drop ./design.svg
[chat] dropped design.svg (148 bytes)
```

`/drop <path>` hashes the file into a local `iroh-blobs` `MemStore`, broadcasts a tiny gossip Message announcing the hash, then peers fetch the bytes out-of-band over the iroh-blobs ALPN against your NodeId. Both peers' Claudes see it as `@file:<path>` on the next prompt — Claude Code reads it via its native file-attach convention.

**v0.2 cap: 1 GiB per file**, set by `FILE_DROP_MAX_BYTES` in `cc-connect-core::message`. Bytes flow via iroh-blobs, not gossip, so there's no per-frame envelope to budget against. Files only persist for the lifetime of your `cc-connect chat` process (the store is in-memory) — once you exit, late joiners can't fetch what you dropped.

### What Claude sees

While `cc-connect chat …` is running, every prompt you send to Claude Code in another pane has the recent unread chat lines spliced into Claude's context. Claude doesn't know there's a chat — to it, the lines just look like extra prompt context tagged `[chatroom @nick HH:MMZ] body`.

### Self-hosted relay (optional)

By default cc-connect routes through n0's free public relay cluster (used by every iroh deployment). To run through your own server instead — for privacy, geographic locality, or to avoid n0's rate limits — point at a self-hosted iroh-relay:

```bash
cc-connect host --relay https://relay.yourdomain.com
cc-connect chat <ticket> --relay https://relay.yourdomain.com   # joiners may also override
```

The host's `--relay` URL is baked into the printed ticket, so joiners who use the same ticket pick up the relay automatically — they only need to pass `--relay` themselves to override.

#### Standing the relay up

You need: a Linux server (Debian / Ubuntu tested), nginx + certbot installed, sudo, a (sub)domain with an A record pointing at the server, and Rust toolchain (the skill installs it for you if missing). The repo ships a `cc-connect-relay-setup` skill at `.claude/skills/cc-connect-relay-setup/SKILL.md` that automates the whole thing. Open Claude Code in any directory and paste:

> 帮我用这台服务器自建一个 cc-connect 的 iroh-relay。SSH 是 `user@host`，域名是 `relay.example.com`，邮箱是 `me@example.com`。

Claude will SSH in (key auth required), install `iroh-relay`, issue a Let's Encrypt cert via certbot, write the nginx vhost + systemd unit, and verify the relay returns 200 OK from the open internet. Takes ~5 minutes (most of it is `cargo install iroh-relay`).

If you'd rather do it by hand, the manual steps live in [`.claude/skills/cc-connect-relay-setup/SKILL.md`](.claude/skills/cc-connect-relay-setup/SKILL.md) — copy each `ssh <target> '…'` block into your terminal.

#### What runs where

```
your-laptop                   your-server                          their-laptop
                              ┌────────────────────────┐
cc-connect chat ─────────────►│ nginx :443 (TLS)       │◄───────── cc-connect chat
   (ticket has relay URL)     │ ▼ proxy 127.0.0.1:8443 │              (same relay)
                              │ iroh-relay (systemd)   │
                              └────────────────────────┘
```

iroh-relay sees only QUIC-encrypted traffic; it cannot read message contents (BLAKE3 + per-session keys). It does see NodeId pairs + traffic volume.

### Configure your displayed name (optional)

Create `~/.cc-connect/nicknames.json`:

```json
{
  "hnvcppgow2sc2yvdvdicu3ynonsteflxdxrehjr2ybekdc2z3iuq": "alice",
  "k7p8mfx9rsa3jzwh4ab5n6tdgfk2tmvc8eyhbjr1ympd5fnl2quz": "bob"
}
```

Maps Pubkey strings (full 52-char base32) to a human-readable nickname. The mapping is local-only — Bob doesn't see what Alice nicknamed him; each peer maintains their own.

---

## Two-laptop demo procedure

For the real magic-moment test:

1. Both machines: complete Setup steps 1-3 above.
2. Alice (machine A): `cc-connect host` in tmux right pane. Copy the printed `cc1-…` code.
3. Alice: in tmux left pane, `claude` (Claude Code).
4. Bob (machine B): paste the code into `cc-connect chat <code>` in tmux right pane.
5. Bob: in tmux left pane, `claude`.
6. **Bob types into his chat pane**: `try sqlite for now`
7. **Alice asks her Claude something** (anything). On submit, the hook reads Bob's message from Alice's local log and injects it as context. Alice's Claude reply should reference Bob's suggestion.

If it doesn't work, see [Troubleshooting](#troubleshooting).

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `cc-connect host` hangs at "binding endpoint" | Firewall blocks n0's relay servers | Try a different network. Real LAN-only mode is v0.2+. |
| `cc-connect chat` says `Joined room … (peers: 1)` but no messages flow | mDNS is blocked (corporate WiFi client isolation) | Try a coffee-shop / home network. |
| Hook silently does nothing | Settings.json hook path is relative, or binary not on PATH | Use absolute path; restart Claude Code; `cc-connect doctor` |
| Late joiner sees `[chatroom] (joined late, no history available)` | Backfill request to first peer timed out (5 s) | Confirm both peers are reachable; v0.1 doesn't retry across peers, that's a v0.2 polish |
| `cargo build` fails on `ed25519-3.0.0-rc.4` | Missing `[patch.crates-io]` (you cloned without `vendored/`) | Re-clone or `git fetch origin main && git reset --hard origin/main` |
| Identity file mode wrong | Drifted from 0600 | `chmod 600 ~/.cc-connect/identity.key` (doctor warns) |
| `/tmp/cc-connect-$UID/active-rooms/` mode wrong | Loose perms | `rm -rf "$TMPDIR/cc-connect-$UID/" && cc-connect chat …` |

If `cc-connect-hook` fired but you suspect it failed, check `~/.cc-connect/hook.log`. The hook always exits 0 (PROTOCOL §7.4) so error don't propagate to Claude.

---

## Project layout

```
cc-connect/
├── PROTOCOL.md              v0.1 wire-and-disk specification
├── CONTEXT.md               Domain glossary (DDD-style)
├── docs/adr/                Architecture decision records (1-4)
├── crates/
│   ├── cc-connect-core/     Protocol primitives library (71 tests)
│   ├── cc-connect/          host / chat / room / host-bg / doctor binary
│   ├── cc-connect-tui/      TUI binary (cc-connect-tui) + library
│   └── cc-connect-hook/     UserPromptSubmit hook binary
├── tests/                   FAKE-CLAUDE-CODE integration test
├── vendored/                Patched ed25519 + ed25519-dalek (temporary,
│                            see TODOS.md and curve25519-dalek#901)
└── spike/                   Spike 0 evidence (hook byte-cap probe)
```

---

## Status / contributing

v0.1 is feature-complete in commits but un-released because of the upstream `ed25519` RC issue. See [`TODOS.md`](./TODOS.md) for the upstream tracker and removal procedure.

Current cadence: protocol-first, every wire-format detail in PROTOCOL.md, tests are byte-exact where it matters.

Issues and PRs welcome on the private repo.
