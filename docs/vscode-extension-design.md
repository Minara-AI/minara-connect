# VSCode extension — design doc

> Status: draft, pre-scaffold. Captures decisions made before writing code.
> Sub-decisions (stream-json over PTY; reuse `host-bg` instead of a new
> daemon) become ADRs once first scaffold validates the assumptions in §9.

## 1. Goal

Native VSCode integration of cc-connect. Open the extension → start or
join a Room → side-by-side **chat panel** + **Claude panel** inside
VSCode, no terminal involved. The chat panel is the primary command
surface; the Claude panel is auxiliary (watch-and-occasionally-intervene).

Out of scope: replacing the CLI / TUI. Both stay first-class. The
extension is an additional client of the same Substrate.

Vocabulary in this doc follows @CONTEXT.md verbatim — Room, Peer,
Substrate, Hook, Cursor, Session, Identity, Pubkey, Nickname, Backfill,
Injection, Context. The running `claude` driven by the extension is
the **embedded Claude** of the Peer's Session.

## 2. Architecture

```
┌─────────────────── VSCode window ───────────────────┐
│ Sidebar: Rooms     │  Editor-area webview            │
│ ┌────────────────┐ │  ┌─ chat ─────┐ ┌─ claude ────┐ │
│ │ + Start Room   │ │  │ [bob] hi   │ │ ▸ Read file │ │
│ │ + Join Room    │ │  │ @me build  │ │   src/x.ts  │ │
│ │                │ │  │  …         │ │ ▸ Edit …    │ │
│ │ • team-A  (H)  │ │  │            │ │ [approve?]  │ │
│ │ • design       │ │  │ ›          │ │             │ │
│ └────────────────┘ │  └────────────┘ └─────────────┘ │
└─────────────────────────────────────────────────────┘
```

Per-Room runtime, lives in the VSCode **extension host** process:

1. `cc-connect host-bg start` — reused as-is, owns gossip / chat-daemon /
   `chat.sock` / `log.jsonl` / `events.jsonl` for that Room.
2. Extension host connects to `chat.sock` for chat send/receive, and
   tails `log.jsonl` (Messages) + `events.jsonl` (rate-limit warnings,
   system notices) for rendering.
3. `claude --print --input-format stream-json --output-format stream-json
   --include-hook-events --verbose --session-id <stable-UUID>
   --mcp-config <inline>` — spawned **per turn** (per @-mention), not
   long-running. Each turn is a fresh `--print` invocation; the
   extension threads continuity through `--session-id`, which is
   honored end-to-end (verified §9 Test 2). No API key; uses the
   user's Claude subscription via OAuth, same as the CLI. Each child's
   env **MUST** include `CC_CONNECT_ROOM=<topic-hex>` so the existing
   `UserPromptSubmit` hook gates injection correctly (see §3).
   `--include-hook-events` is required so the Claude panel can render
   hook activity (hook_started / hook_response with stdout / stderr /
   exit_code) without extra plumbing.
4. cc-connect MCP server registered into the spawned `claude` via
   `--mcp-config` so `cc_send` / `cc_drop` / `cc_wait_for_mention` work
   inside the embedded Claude. The MCP entry shape is the same one
   `setup.rs` writes to `~/.claude.json` under key `cc-connect` (see
   `lifecycle.rs::MCP_SERVER_KEY`); the extension constructs an
   equivalent entry in-memory rather than reading the user's global
   `~/.claude.json` (avoids coupling to the user's Claude config).
5. The existing `UserPromptSubmit` hook fires on every prompt the
   extension submits over stdin — so unread chat injection (with the
   PROTOCOL §7.3 orientation preamble) happens automatically. The
   extension never hand-rolls Context concatenation. **Validated**
   §9 Test 1.
6. The hook receives a JSON payload on stdin containing `session_id`,
   `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`,
   `prompt`. The extension does not need to inject these — Claude Code
   does. The `transcript_path` is the per-Session JSONL transcript
   under `~/.claude/projects/...`; the extension MAY read it for
   in-panel deep links but MUST NOT modify it.

### 2.1 Isolation contract (trust boundary)

The new client surface MUST NOT regress the cross-process isolation
guarantee called out in @SECURITY.md and CLAUDE.md.

- **Webview is a sandbox, not a participant.** The webview runs untrusted
  HTML and renders untrusted peer chat — it MUST NEVER open `chat.sock`,
  `log.jsonl`, or `~/.cc-connect/identity.key` directly. All Substrate
  I/O goes through the extension host process, mediated by the
  `postMessage` protocol in §4.4.
- **Per-Room scoping.** The extension host opens one chat.sock + one
  embedded Claude per active Room view. It MUST NOT proxy Messages
  across Rooms even within the same VSCode window.
- **`CC_CONNECT_ROOM` env discipline.** Each spawned `claude` child
  gets `CC_CONNECT_ROOM=<topic-hex>` set in its env, identical to what
  `cc-connect-tui` does today. Unrelated `claude` invocations on the
  same machine — including any other VSCode extension that spawns
  `claude` — see no chat. Don't loosen.
- **Webview Content-Security-Policy.** The webview MUST forbid
  `unsafe-inline`, forbid all remote origins, and render chat bodies
  as text nodes (never `innerHTML`). Mention highlighting is
  whitelist-only. This is a hard requirement because peer chat is
  treated as untrusted-content per the orientation preamble.

## 3. Pinned decisions

### D1 — Chat → embedded Claude: only `@me` triggers a Session turn

**Rule.** Plain chat is broadcast to peers and rendered in the chat
panel. Only an `@<my-nick>` mention triggers a new turn on the embedded
Claude. Self-instruction = type `@me <task>` in your own chat.

**Why.** Aligns with the existing `cc_wait_for_mention` MCP tool and
the peer-@-mention wake feature already in the repo. Avoids spamming
the embedded Claude with peer chatter. Keeps "talk to peers" and
"command my Claude" visually identical (one chat panel) but
behaviorally distinct (one trigger word).

**How.**
- **Extension-orchestrated, per-turn spawn.** The extension tails
  `log.jsonl` / `events.jsonl` and applies `chat-ui/src/mention.ts`
  to detect `@<my-nick>`. On detection, the extension spawns a fresh
  `claude --print --session-id <stable-UUID> --input-format stream-json
  --output-format stream-json --include-hook-events --verbose
  --mcp-config <inline>` with the mention's body as the user prompt.
  The hook injects unread chat (now including the just-mentioned
  Message); Claude responds, may call MCP tools (`cc_send` to reply
  back into the Room, `cc_drop` to share a file), then exits. Next
  mention → next spawn, same `--session-id` so Claude Code's
  per-session prompt cache and the (Room, Session) Cursor both stay
  coherent.
- **`cc_wait_for_mention` is NOT used by the extension.** The MCP
  tool stays for plain CLI / TUI users (where claude is long-running
  and self-polls). Extension-side detection + per-turn `--print` is
  simpler, has no 600s blocking-tool concern, and lets the extension
  queue / cancel turns without round-tripping through MCP.
- **Mid-turn arrivals.** If `@me bar` arrives while a `--print` is
  still running for `@me foo`, the extension queues `bar` and renders
  "Claude busy — N queued mentions" in the chat panel. When `foo`'s
  process exits (clean, error, SIGTERM), the extension dequeues and
  spawns the next turn. Cancellation in v0 = SIGTERM the in-flight
  `--print` (closing the tab per D2 does this automatically); a
  richer cancel UX is in §8.
- **One Session per Room view.** Same `--session-id` across all
  spawns within a tab's lifetime. §4.1 covers Cursor implications.

### D2 — Tab/window close: kill embedded Claude, prompt for host-bg

**Rule.** Closing a Room view in the extension always stops that Room's
embedded `claude` subprocess. If the user originated this Room's
`host-bg` daemon, the extension prompts whether to also stop the
daemon (default: keep running, so peers can still join via the ticket).

**Why.** Mirrors `cc-connect-tui`'s `Ctrl-W` semantics one-for-one.
Keeps a single mental model across CLI + TUI + VSCode: "closing the
local view ≠ tearing down the Room."

**How.**
- On user-initiated tab close: SIGTERM the embedded Claude. If the
  Peer owns the `host-bg` daemon, show a modal — `Stop daemon` /
  `Keep running` (default). Reuse `cc-connect host-bg list` and
  `cc-connect host-bg stop <prefix>`.
- On extension *deactivation* (window close, reload, crash): SIGTERM
  the embedded Claude; the modal cannot fire reliably here, so fall
  back to the user's stored preference (default: keep host-bg
  running, matching `cc-connect clear` semantics). Preference key
  lives in `~/.cc-connect/extension/config.json` per §6, not in
  VSCode `globalState`.
- **Mid-tool-use SIGTERM.** Partial writes are accepted as v0
  behaviour — same cost as Ctrl-C in TUI today. Document, don't fix
  in v0.

## 4. v0 implementation contract

These are the load-bearing implementation choices. Each one is small
on its own; together they make or break behavioral parity with the TUI.

### 4.1 Sessions and Cursors

- One Room view = one Session. The Session has one Cursor (per
  CONTEXT.md, Cursors are per-(Room, Session)). `session_id` is the
  one passed to the Hook on every prompt.
- Closing the tab terminates the Session; reopening creates a new
  `session_id` → fresh Cursor → backfill re-renders. This is **not a
  bug**; it matches PROTOCOL §9. State this in the UI ("This tab is a
  new Session — re-injecting recent history").
- The extension MUST pass `--session-id <stable-UUID>` to **every**
  `--print` invocation within one tab's lifetime, so the Cursor
  advances correctly across the per-turn process boundary. Verified
  end-to-end in §9 Test 2 — the UUID threads through to the hook's
  stdin payload verbatim, which is what cc-connect-hook keys its
  Cursor on.

### 4.2 Persistent state

VSCode extensions can write to `globalState` / `workspaceState` / a
`globalStorageUri`. The cleanup contract from CLAUDE.md
("`cc-connect uninstall --purge` ends with zero cc-connect-touched
state") only knows about `~/.cc-connect/`. Therefore:

- **cc-connect-protocol state** (Rooms list, last-used Room, chosen
  relay, Identity, Nickname) → `~/.cc-connect/` only. Reuses the
  existing `~/.cc-connect/config.json` for Nickname (`self_nick`),
  same key the TUI uses. Extension-specific keys go in a new
  `~/.cc-connect/extension/config.json` so `lifecycle.rs::run_uninstall
  --purge` reaches it (already wipes `~/.cc-connect/`, so no new
  removal step needed — but `lifecycle.rs` MUST be updated to mention
  the file in its accounting comment).
- **VSCode UI ergonomics state** (last panel split ratio, sidebar
  width) → `workspaceState` is fine; lifetime is the workspace, which
  the user owns. Forbidden for anything that survives an
  uninstall.

### 4.3 Identity, Nickname, and OAuth

- **Identity.** ADR-0001: one machine = one key at
  `~/.cc-connect/identity.key`. The extension does not generate or
  read this directly — it shells out to `cc-connect host-bg start`,
  which uses the existing loader.
- **Nickname.** Read `self_nick` from `~/.cc-connect/config.json`. If
  unset, the extension prompts on first Room open and writes back
  through the same path the TUI uses.
- **OAuth.** Headless `claude` reads the user's subscription token
  from the same path the CLI does (currently `~/.claude/credentials`
  or platform keychain — depends on the user's OS). The extension
  host's `$HOME` MUST match the one the user OAuth'd `claude` in.
  On macOS this is automatic. **§9 validation point.**

### 4.4 PATH resolution on macOS GUI launch

When VSCode is launched from Spotlight / Finder / Dock on macOS, the
extension host inherits launchctl's environment, **not** the user's
shell environment. `claude`, `cc-connect`, `cc-connect-mcp`,
`cc-connect-host-bg` are not on `PATH`.

- The extension MUST resolve binaries by absolute path:
  `~/.local/bin/<name>` (where `install.sh` symlinks them per
  `INSTALLED_BIN_NAMES`).
- If the symlink is missing, `cc-connect doctor`-style guidance is
  surfaced in the sidebar instead of letting `spawn` fail with
  `ENOENT`.
- Document this in the extension README and the troubleshooting
  table in @README.md once shipped.

### 4.5 MCP config for the embedded Claude

- The extension constructs an in-memory MCP config equivalent to the
  one `setup.rs` writes to `~/.claude.json::mcpServers["cc-connect"]`.
  Key shared with `lifecycle.rs::MCP_SERVER_KEY`.
- Keep the construction in **one place** in the extension code,
  named `MCP_SERVER_KEY = "cc-connect"`, so future protocol-level
  renames stay symmetric across `setup.rs` / `lifecycle.rs` / extension.
  Add a CI grep-check (or doc-only TODO) to catch drift.

### 4.6 Orientation preamble

Per PROTOCOL §7.3 step 6b, the reference Hook implementation prepends
a multi-line orientation preamble (Room name, MCP tools available,
trust boundary, `for-you` directive) before the chat block. Because
the extension submits prompts to the **same** `claude` binary that
runs the same Hook, the embedded Claude receives the same preamble
unchanged. No extension-side work; this falls out of §2 step 5.

### 4.7 Webview ↔ extension-host postMessage protocol (skeleton)

Spec'd fully when scaffolding lands; named here so reviewers know the
surface exists and is bounded:

| Direction | Type | Payload |
|---|---|---|
| webview → host | `chat:send` | `{ body: string }` |
| webview → host | `claude:approve` | `{ request_id, decision }` |
| webview → host | `room:cancel-turn` | `{}` (deferred — see §8) |
| host → webview | `chat:message` | the Message verbatim from `log.jsonl` |
| host → webview | `chat:event` | rate-limit / system notice from `events.jsonl` |
| host → webview | `claude:event` | a stream-json event from the embedded Claude |
| host → webview | `room:state` | peer count, busy banner, queued mentions |

The webview never receives raw bytes from `chat.sock` or the embedded
Claude's stdout — the host re-emits them as typed messages so the
webview's CSP can stay strict.

## 5. Reuse strategy

- `chat-ui/src/{ipc.ts,log_tail.ts,mention.ts,ticket.ts,types.ts,
  theme.ts}` are renderer-agnostic (verified — no Ink imports). Lift
  directly into the extension. The Ink `components/` need a DOM
  rewrite; business logic transfers as-is.
- `cc-connect-mcp` — registered into the embedded Claude via
  `--mcp-config`. No fork.
- `cc-connect-hook` — unchanged. Continues to be the canonical
  Injection path for both TUI and extension.
- Open: whether `chat-ui/` and the extension share a TS package vs.
  vendor-by-symlink. Decide at scaffold time.

## 6. Lifecycle obligations

CLAUDE.md release checklist applies to this extension as if it were a
new crate. The contract: `cc-connect uninstall --purge` reaches **all**
extension-written state.

- New persistent file paths added by the extension: list them in
  `lifecycle.rs` (in the accounting comment, not as a removal step,
  if they live under `~/.cc-connect/`).
- New binaries: none in v0 (the extension is a TS package, not a
  native binary).
- New `~/.claude/settings.json` keys: none — the extension reuses the
  Hook entry that `setup.rs` already writes.
- New MCP server entries: none — reuses the `cc-connect` entry.
- **Marketplace asymmetry** (acknowledged, not solved in v0):
  `cc-connect uninstall` cannot remove a Marketplace-installed
  extension; the VSCode "Extensions: Uninstall" command cannot remove
  cc-connect's Rust binaries. Each side prints a notice pointing at
  the other:
  - `cc-connect uninstall` exit message: "VSCode extension still
    installed — remove via `Extensions: Uninstall cc-connect`."
  - Extension `deactivate()`: offer to run `cc-connect clear`.

## 7. Layout

New top-level directory `vscode-extension/` (sibling of `chat-ui/`,
`crates/`). TypeScript + Vite + React for the webview. `package.json`
with VSCode contribution points. Published from the monorepo so
protocol changes land in one PR with the matching client.

Minimum VSCode API version pinned to whatever ships
`vscode.window.tabGroups.onDidCloseTab` (≥ 1.74, very old). Locked
explicitly in the extension `package.json` `engines.vscode`.

## 8. Open questions (deferred)

- **File drop UX.** Right-click in editor → "Drop to Room" vs
  chat-panel `/drop` slash command. Likely both; primary picked
  later.
- **Permission approvals UI.** Modal vs inline buttons in Claude
  panel. Both work over stream-json `permission_request`; pick on UX
  feel.
- **Multi-Room layout.** One webview per Room (multiple editor tabs)
  vs single webview with internal tab strip. Likely the former.
- **In-band turn cancellation** beyond "close the tab". Want a
  Cmd-Period analog that interrupts the embedded Claude without
  killing the Session.
- **Existing TUI Room collision.** If the user has
  `cc-connect room start` running in a TUI and opens VSCode, does
  the extension attach to the existing `host-bg` (by topic), refuse,
  or spawn a duplicate? `host-bg list` makes attach plausible.
- **`cc-connect doctor` integration.** Should `doctor` know the
  extension is installed (so its "all good" output reflects reality)?
  Probably yes; deferred to first scaffold.
- **Marketplace publish target.** Marketplace + Open VSX, or
  ship-from-source first.

## 9. Validation results

Tests 1, 2, 4 ran 2026-05-02 against `claude` v2.1.126 on macOS 24.6.
Tests 3, 5 deferred (rationale below). Captures live under
`/tmp/cc-smoke/` for the duration of the scaffold work.

1. ✅ **Hook fires under `--print --input-format stream-json
   --output-format stream-json --include-hook-events --verbose`.** Both
   cc-connect-hook entries in `~/.claude/settings.json` ran, returned
   `exit_code: 0`, and emitted full `hook_started` → `hook_response`
   pairs in the output stream. Capture: `/tmp/cc-smoke/test1.jsonl`.

2. ✅ **`--session-id <UUID>` threads through to hook stdin.** Passing
   `12345678-1234-1234-1234-123456789abc` resulted in that exact UUID
   appearing in (a) every stream-json event and (b) the JSON payload
   on the hook's stdin (verified via abort-sentinel hook capturing
   stdin to a file). Cursor key is stable across `--print`
   invocations as long as the extension threads the same
   `--session-id`. Test cost zero quota (`duration_api_ms: 0`,
   `total_cost_usd: 0`) — sentinel hook returned `{"continue":false}`
   to abort before any model call. Capture:
   `/tmp/cc-smoke/hook-stdin.json`.

3. ⏭ **Deferred — `cc_wait_for_mention` long-timeout under
   stream-json.** Per the D1 revision the extension does not call
   this MCP tool at all; CLI / TUI users do, against the same Claude
   Code engine that already works for them today. No longer a
   load-bearing assumption.

4. ✅ **Headless OAuth.** Tests 1 + 2 ran headlessly under `--print`
   from a non-interactive Bash subshell using the user's existing
   subscription. `init` event reported `apiKeySource: "none"`, and
   the call succeeded — confirming OAuth is the resolution path.
   No API key involved.

5. ⏭ **Deferred — PATH on macOS GUI launch.** Cannot validate
   without the extension scaffold actually running inside a
   GUI-launched VSCode. Mitigation in §4.4 (resolve every binary by
   absolute path under `~/.local/bin/`) stands and is implementable
   without this validation.

**Bonus findings adopted into the design**:

- `--include-hook-events` provides full hook-lifecycle observability
  in the output stream (hook_id, name, stdout, stderr, exit_code,
  outcome). Folded into §2 step 3 and the §4.7 protocol skeleton —
  the Claude panel renders these directly.
- Hook stdin payload includes `transcript_path` and
  `permission_mode`; the extension may read `transcript_path` for
  history deep-links (§2 step 6).
- `--setting-sources project` cleanly isolates user-global hooks for
  testing — useful in extension dev / smoke-test scripts (not in
  production extension behaviour, where we want user hooks to run).
- Per-turn `--print` invocation pays a system-prompt
  `cache_creation` cost on cold start (~37k tokens observed in
  Test 1, against the user's subscription quota, not a per-call
  charge). `--session-id` resumption is expected to convert these
  into `cache_read` after the first turn; needs measurement once
  scaffold lands. Note this in performance benchmarks.

**Fall-back path (no longer load-bearing)**: if a future Claude Code
release changes the `--include-hook-events` contract or stops
threading `--session-id` to the hook, the extension hand-rolls
Injection in TS by tailing `log.jsonl` and prepending unread Messages
to the prompt before submitting. Tracked as a §8 open question, not
a v0 requirement.
