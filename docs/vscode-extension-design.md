# VSCode extension — design doc

> **Status: v0 shipped (vscode-extension-v0.2.1, 2026-05-05).** This
> doc captures the decisions made *before* writing code. Sections 1-7
> are still load-bearing — the implementation honors them. Section 8
> (open questions) and §9 (pre-scaffold validation) are annotated
> below to reflect what was resolved during the build-out. For *what
> the extension does today*, read [`vscode-extension/CLAUDE.md`](../vscode-extension/CLAUDE.md);
> this doc is the historical "why".

## 0. What shipped

The v0 contract in §4 was implemented end-to-end and ships in
`vscode-extension-v0.2.1`. Beyond v0, the following polish landed in
the same release window (2026-05) — none of it changes the §1-§7
architecture:

- **Permission-mode pill + inline approval bubbles**, deferred per §8
  ("Permission approvals UI") and resolved with a four-state pill
  (`auto` / `ask edits` / `plan` / `ask all`) plus inline Allow / Deny
  / Always-allow buttons backed by SDK `canUseTool`. See
  [`webview/PermissionBubble.tsx`](../vscode-extension/webview/PermissionBubble.tsx)
  and [`src/host/claude_runner.ts`](../vscode-extension/src/host/claude_runner.ts).
- **Setup walkthrough + viewsWelcome state**, resolved per §8
  ("`cc-connect doctor` integration"). Activation runs
  [`src/host/binaryVersion.ts::checkCcConnectBinary`](../vscode-extension/src/host/binaryVersion.ts);
  failure routes the user to a 4-step walkthrough at
  [`media/walkthrough/`](../vscode-extension/media/walkthrough/).
- **Conversation history per workspace**, reading
  `~/.claude/projects/<encoded-cwd>/*.jsonl` directly (see
  [`src/host/transcripts.ts`](../vscode-extension/src/host/transcripts.ts)).
- **Launcher-parity prompts** — the extension's Claude runner now
  feeds the same `bootstrap-prompt.md` + `auto-reply-prompt.md` that
  the TUI's `claude-wrap.sh` uses, so the embedded Claude knows it's
  in a Room and auto-greets on join.
- **Copy-ticket pill** in the room-meta strip (read from the
  chat-daemon.pid JSON).
- **IM-style chat layout** (own messages right-aligned with bubbles)
  + per-tool codicons + IN/OUT-split tool cards.

What's *still* deferred (post-v0):
- Marketplace publish (Stage B) — needs Azure DevOps PAT + verified
  publisher; tracked in
  [`.claude/skills/publish/SKILL.md`](../.claude/skills/publish/SKILL.md).
- Multi-Room webview — still single-Room per WebviewView. The §8
  discussion ("multiple editor tabs vs internal tab strip") remains
  open.
- TUI-Room collision detection — opening VSCode while a TUI Room is
  active still spawns a parallel `host-bg` for the topic; the §8
  question about "attach to existing daemon by topic" is open.

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
3. `query()` from `@anthropic-ai/claude-agent-sdk` (formerly the Claude
   Code SDK) — spawns the user's installed `claude` binary under the
   hood, exposes a typed `AsyncGenerator<SDKMessage>`, and threads
   continuity through a stable `sessionId` option. The extension does
   **not** hand-roll subprocess spawn / stream-json parsing /
   `--mcp-config` JSON wrangling — the SDK does all of that. License
   note: SDK is under Anthropic Commercial Terms (not OSS); used as a
   runtime npm dependency, same pattern as `@anthropic-ai/sdk` in OSS
   projects. No redistribution of SDK source.

   Per turn (per @-mention) the extension calls `query()` with a
   prompt and an `AbortController`. The SDK supports streaming input
   (`AsyncIterable<SDKUserMessage>`) so multi-prompt-per-call is also
   possible if we need it later. Each invocation's env **MUST** include
   `CC_CONNECT_ROOM=<topic-hex>` so the existing `UserPromptSubmit`
   hook gates injection correctly (see §3). The `includeHookLifecycleEvents`
   option emits hook_started / hook_response into the same typed event
   stream so the Claude panel can render hook activity natively.
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

### D1 — Chat → embedded Claude: explicit AI address triggers a turn

**Rule.** Plain chat is broadcast to peers and rendered in the chat
panel. A new turn on the embedded Claude fires only when the message
body explicitly addresses the AI:

- `@<my-nick>-cc` — the AI mirror form (your own AI, or a peer's AI)
- `@cc` / `@claude` / `@all` / `@here` — broadcast tokens (every
  participating AI in the Room)

**Bare `@<my-nick>` does NOT wake the local Claude.** That form
addresses the *human* — peers chatting "yo @yjj seen this?" should
not auto-summon yjj's Claude. Self-instruction = type
`@<my-nick>-cc <task>` in your own chat.

**Why.** Deliberate narrowing from the Rust
`hook_format::mentions_self` (which DOES match bare `@<self>` for
its `for-you` directive). Distinction: that hook is *passive context
injection on an already-running turn*; D1 here gates the *active
spawn of a fresh `query()`*. Different operations, different rules.
Avoids auto-summoning the AI on every casual `@yjj` from peers and
keeps "talk to peers" vs "command my Claude" cleanly separable.

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
- On user-initiated tab close: call `abortController.abort()` on the
  in-flight `query()` (and `q.interrupt()` if mid-streaming-input);
  the SDK cleanly tears down its child `claude` process. If the
  Peer owns the `host-bg` daemon, show a modal — `Stop daemon` /
  `Keep running` (default). Reuse `cc-connect host-bg list` and
  `cc-connect host-bg stop <prefix>`.
- On extension *deactivation* (window close, reload, crash):
  `abortController.abort()` again; the modal cannot fire reliably
  here, so fall back to the user's stored preference (default: keep
  host-bg running, matching `cc-connect clear` semantics).
  Preference key lives in `~/.cc-connect/extension/config.json` per
  §6, not in VSCode `globalState`.
- **Mid-tool-use abort.** SDK's abort path lets the in-flight tool
  call return cleanly when possible; for non-cooperating tools
  (e.g. an Edit mid-write) partial writes are accepted as v0
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

- For `claude`: pass
  `pathToClaudeCodeExecutable: '<homedir>/.local/bin/claude'` to
  `query()`. SDK handles the rest.
- For the cc-connect binaries (`cc-connect`, `cc-connect-host-bg`,
  `cc-connect-mcp`): the extension resolves by absolute path
  `~/.local/bin/<name>` (where `install.sh` symlinks them per
  `INSTALLED_BIN_NAMES`).
- If any symlink is missing, surface `cc-connect doctor`-style
  guidance in the sidebar instead of letting `spawn` fail with
  `ENOENT`.
- Document this in the extension README and the troubleshooting
  table in @README.md once shipped.

### 4.5 MCP config for the embedded Claude

- The extension passes `mcpServers` directly to the SDK's `query()`
  options — no `--mcp-config` JSON file or string wrangling. The
  shape mirrors the entry `setup.rs` writes to
  `~/.claude.json::mcpServers["cc-connect"]`; `MCP_SERVER_KEY`
  ("cc-connect") stays a single shared constant.
- Construction lives in one place in the extension code, named
  `MCP_SERVER_KEY = "cc-connect"`, so future protocol-level renames
  stay symmetric across `setup.rs` / `lifecycle.rs` / extension. Add
  a CI grep-check (or doc-only TODO) to catch drift.

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
| host → webview | `claude:event` | one `SDKMessage` from the SDK's `AsyncGenerator` (already typed; no parsing) |
| host → webview | `room:state` | peer count, busy banner, queued mentions |

The webview never receives raw bytes from `chat.sock` or the embedded
Claude's stdout — the host re-emits them as typed messages so the
webview's CSP can stay strict.

## 5. Reuse strategy

- `chat-ui/src/{ipc.ts,log_tail.ts,mention.ts,ticket.ts,types.ts,
  theme.ts}` are renderer-agnostic (verified — no Ink imports). Lift
  directly into the extension. The Ink `components/` need a DOM
  rewrite; business logic transfers as-is.
- `@anthropic-ai/claude-agent-sdk` (npm) — runtime dep that wraps
  spawn / stream-json / hook events / abort / MCP injection /
  permission requests. Replaces what would otherwise be ~hundreds of
  lines of subprocess + parser glue. Anthropic Commercial Terms;
  used as a normal npm dep alongside our MIT/Apache extension code.
- `cc-connect-mcp` — registered into the embedded Claude via the
  SDK's `mcpServers` option. No fork.
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

## 8. Open questions

Annotated 2026-05-05 with resolution status. Items marked ✅ shipped;
☐ remain open; ⏭ deferred with rationale.

- ✅ **File drop UX.** Right-click in editor → "Drop to Room" vs
  chat-panel `/drop` slash command. **Resolved**: shipped both — the
  chat pane has a `+` attach button (opens `vscode.window.showOpenDialog`
  → `ccDrop`) and a `/` slash launcher with `/drop`. No editor-context
  right-click yet.
- ✅ **Permission approvals UI.** Modal vs inline buttons. **Resolved
  inline** — `PermissionBubble.tsx` renders Allow / Deny / Always-allow
  in the Claude log, gated by a four-state mode pill (`auto` /
  `ask edits` / `plan` / `ask all`). The SDK's `canUseTool` callback
  is only attached when the user is in `ask all` mode; the other modes
  short-circuit at the SDK level (avoids the headless ZodError pitfall
  noted in [`vscode-extension/CLAUDE.md`](../vscode-extension/CLAUDE.md)).
- ☐ **Multi-Room layout.** One webview per Room (multiple editor tabs)
  vs single webview with internal tab strip. **Still open** — v0
  ships single-Room-per-WebviewView, switching by re-pointing
  `setRoom(topic)`. The internal-tab approach was deferred when the
  Rooms tree's "switch by clicking" UX proved sufficient for v0.
- ✅ ~~In-band turn cancellation~~ — resolved by SDK's `q.interrupt()`
  + `abortController.abort()`. Shipped as the Stop button (T1.2,
  commit `ede84cc`).
- ☐ **Existing TUI Room collision.** If the user has
  `cc-connect room start` running in a TUI and opens VSCode, does
  the extension attach to the existing `host-bg` (by topic), refuse,
  or spawn a duplicate? **Still open** — current behavior:
  `chat-daemon start <ticket>` returns `ALREADY <topic> <pid>` so the
  extension reuses the existing daemon (good); but `host-bg start` is
  always spawned fresh which can race with a TUI host on the same
  topic. Tracked as a v0.x cleanup.
- ✅ **`cc-connect doctor` integration.** Should `doctor` know the
  extension is installed? **Resolved differently** — instead of
  `doctor` learning about the extension, the *extension* runs
  `cc-connect --version` at activate (see
  [`src/host/binaryVersion.ts`](../vscode-extension/src/host/binaryVersion.ts))
  and gates Start/Join Room with a friendly "binary too old" toast if
  the version is below `MIN_CC_CONNECT_VERSION`. Bidirectional
  awareness wasn't needed for v0; one-way binary→extension version
  pinning suffices.
- ⏭ **Marketplace publish target.** Marketplace + Open VSX, or
  ship-from-source first. **Deferred to Stage B** — `.vsix` is
  attached to GitHub Release (via
  [`.github/workflows/vscode-extension-release.yml`](../.github/workflows/vscode-extension-release.yml)).
  Marketplace publish needs an Azure DevOps PAT + verified publisher;
  the publish skill's Stage B note documents the wire-up when ready.

## 9. Validation results

> **Historical** — these tests gated the start of scaffold work in
> 2026-05-02. They've been overtaken by real shipping behavior, so
> they aren't a regression suite. Kept for the rationale they
> captured (in particular the §4.4 PATH-on-macOS-GUI mitigation and
> the §2 step 6 `transcript_path` adoption). Live behavior is in the
> codebase + smoke tests, not here.

Tests 1, 2, 4, 6 ran 2026-05-02 against `claude` v2.1.126 +
`@anthropic-ai/claude-agent-sdk` v0.2.126 on macOS 24.6. Tests 3, 5
deferred (rationale below). Captures live under `/tmp/cc-smoke/`
and `vscode-extension/scripts/probe-sdk.ts` for the duration of the
scaffold work.

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

6. ✅ **Claude Agent SDK works end-to-end with OAuth + a
   user-supplied claude binary.** `query()` from
   `@anthropic-ai/claude-agent-sdk@0.2.126`, called from a Bun
   subprocess (proxy for VSCode extension host), spawned the user's
   `~/.local/bin/claude`, produced a `system:init` event with a
   valid `session_id`, and reported `apiKeySource: "none"` —
   confirming OAuth subscription path with no API key. Probe used
   `pathToClaudeCodeExecutable` because the SDK's optional bundled
   native binary (`@anthropic-ai/claude-agent-sdk-darwin-arm64`)
   failed to extract on install — irrelevant since we explicitly
   reuse the user's installed `claude` per §4.4. AbortController
   cleanly shut the SDK down before any model call landed (zero
   quota burn). Capture: `vscode-extension/scripts/probe-sdk.ts`.

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
