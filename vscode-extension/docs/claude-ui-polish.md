# Claude pane UI optimization plan

A standalone TODO that takes inspiration from the visible behavior of
Anthropic's official Claude Code VSCode extension (closed source,
**design patterns only — no code lift**) and adapts each pattern to
cc-connect's Claude pane (`vscode-extension/webview/Claude.tsx` +
`processClaude.ts`).

Read context: `vscode-extension/CLAUDE.md`,
`vscode-extension/docs/vscode-extension-design.md`. Today's pane
already has: tool cards, hook collapse, markdown render, turn
separators, direct prompt input, busy + queue badge, stop on tab
close. This document covers what's missing.

## Status legend

- ☐ not started
- ◐ partially done
- ☑ done

## Tier 1 — high-impact, small-to-medium effort

### ☑ T1.1 — Step-list timeline with vertical connector — `96f1113`

**Pattern observed**: Each Claude turn renders as a vertical timeline.
A solid line connects step bullets (●), each step is a "thought" / a
tool call / an assistant text / a result. Bullets are colored by
state (gray pending, green done, red failed).

**Why**: turns the current flat list of `claude-row` divs into a
scannable causal trail. Easier to see "Claude thought for 2s, then
ran Bash, then wrote a reply".

**How**:
- In `Claude.tsx`, wrap each `BlockRow` in a `.step` container with
  a left padding for the connector
- A pseudo-element `::before` on `.claude-log` draws the vertical
  line via `border-left`
- Each step's bullet is a `::before` on the row itself (small filled
  circle, colored by block state)
- Insert a visible "thought" step when assistant text takes >N
  seconds (timing already in stream events — `system:thinking` events
  if SDK emits them, otherwise derive from gap between turn start
  and first text)

**Effort**: 1–2 hours. Pure CSS + minor JSX wrap.

### ☑ T1.2 — Stop button (interrupt current turn) — `ede84cc`

**Pattern observed**: Red square button next to the prompt input.
Clicking aborts the in-flight turn without closing the pane.

**Why**: today the only way to cancel a turn is to close the pane
entirely (panel.onDidDispose triggers `runner.abort()`). The SDK
already exposes `q.interrupt()` and we already track an
`abortController` per turn — this is wiring, not new mechanism.

**How**:
- Add `cc-connect.interruptClaude` command
- Expose a public `interrupt()` method on `ClaudeRunnerHandle`
- Render a red `<button>` in `.pane-input` that's visible only when
  `state.busy` is true
- Click → postMessage `claude:interrupt` → host calls
  `runner.interrupt()` (or `abortController.abort()` on the
  in-flight query)
- The runner needs to NOT mark the whole panel as aborted — abort
  only the current turn so the queue drains the next one

**Effort**: 1–2 hours.

### ☐ T1.3 — Permission-mode toggle ("Edit automatically" / "Ask before edits")

**Pattern observed**: A toggle pill in the toolbar at the bottom of
the chat: "Edit automatically" | "Ask before edits" | (plan-mode).
Clicking cycles the mode; current mode visible at all times.

**Why**: closes the design-doc §8 deferred "permission UI" item
without writing a full webview-side dialog. The SDK accepts
`permissionMode` per-`query()`, plus `setPermissionMode()` for the
in-flight query.

**How**:
- Add a small `<select>` or pill-toggle in `.pane-head` of the
  Claude pane: `default` / `acceptEdits` / `bypassPermissions` /
  `plan`
- State held in webview React; persisted via `webviewState.setState`
- On change → postMessage `claude:permission-mode` → runner stores
  it as the new default for subsequent `query()` calls
- For the in-flight query, also call `q.setPermissionMode(mode)` if
  available
- If user picks `default` (real prompts), tool calls that need
  approval block with a "Claude wants to <X>… Allow / Deny" inline
  bubble in the Claude log (use the SDK's `canUseTool` callback,
  await a Promise that resolves when the user clicks)

**Effort**: 4–6 hours. The "real prompt" path with inline approval
bubbles is the bulk of the work.

### ☑ T1.4 — "Queue another message…" placeholder + visible queue depth — `ea7441f`

**Pattern observed**: Input placeholder changes to "Queue another
message…" while Claude is busy. The runner currently *does* queue
mid-turn arrivals — but the UI doesn't tell the user that's what's
happening.

**Why**: matches user mental model. We already have the busy +
queued counts in `claudeState`; just plumb them into the placeholder
and add a small queue-pill above the input.

**How**:
- `Claude.tsx` `<textarea placeholder={state.busy ? 'Queue another
  message…' : 'Ask Claude…'} />`
- Above the input area, when `state.queued > 0`, render a small pill
  "N queued · Claude is working on previous prompt"
- No host-side change needed; data is already there

**Effort**: 30 minutes.

### ☑ T1.5 — Thinking indicator ("Thought for Xs") — `8c634b1`

**Pattern observed**: Between an assistant prompt and the assistant's
text reply, a "Thought for Xs" line appears (s = elapsed time before
first text or first tool_use). Updates live until the first content
arrives.

**Why**: latency feedback. Today there's a `· busy` badge but no
sense of *how long* Claude has been thinking on the current step.

**How**:
- In `processClaude.ts`, track timestamps: `system:init` time, time
  of first content block per turn
- Synthesize a `kind: 'thinking'` block with elapsed seconds
- In `Claude.tsx`, render with a small `· thought for Xs ·` style;
  while live, increment via a `setInterval` keyed by turn id

**Effort**: 1–2 hours.

## Tier 2 — medium-impact, medium effort

### ☑ T2.1 — Slash-command launcher (`/` button) — `600ddd8`

**Pattern observed**: A `/` button in the input toolbar opens a
slash-command picker (autocomplete-style).

**Why**: discoverable alternative to typing `/drop ./foo`. Users
don't have to remember syntax.

**How**:
- Add a small icon button next to the textarea in `.pane-input`
- Click → render a popup like `MentionPopup` but listing
  available commands: `/drop`, `/at <nick> <body>`, `/recent`,
  future ones
- Each command has a description; selecting one inserts the
  template into the textarea

**Effort**: 2 hours. Reuse mention popup styling.

### ☑ T2.2 — Attach button (`+`) → file drop dialog — `600ddd8`

**Pattern observed**: The `+` button at the input opens a file
picker.

**Why**: makes `/drop` graphical.

**How**:
- Button next to `/` button
- Click → host posts `vscode.window.showOpenDialog({})` and pipes
  the chosen path through `ccDrop(topic, path)`
- No webview-side complexity beyond the click handler

**Effort**: 1 hour.

### ☐ T2.3 — Conversation history per Room (`⏰` icon)

**Pattern observed**: A history icon opens a list of past Claude
conversations (session by session). Click one → load it into the
pane.

**Why**: today, switching Rooms loses Claude history. Users may
want to revisit "what did Claude do last time in this Room".

**How**:
- Each `query()` writes a transcript JSONL at
  `~/.claude/projects/<cwd-hash>/<session-id>.jsonl` (Claude Code
  default behavior). We can list these per Room (filter by `cwd`).
- Add a "history" icon in `.pane-head`
- Click → load list of session IDs (with their first prompt as a
  preview) → user picks one → load the JSONL into `claudeEvents`
  state and skip live tail for it
- Read-only when viewing history (no input until "new" pressed)

**Effort**: 4 hours. Most of the work is the JSONL → SDKMessage
parser (luckily it's the same shape as live events).

### ☑ T2.4 — "New chat" — fork a fresh Claude session — `4a7e9dc`

**Pattern observed**: A `+` button creates a new conversation, fresh
state.

**Why**: when context gets noisy, user wants a clean slate without
closing the Room.

**How**:
- Add a "fresh session" icon in `.pane-head`
- Click → postMessage `claude:reset-session` → host calls
  `runner.resetSession()` (mints new sessionUuid, clears
  hasStarted, clears event list)
- Webview clears `claudeEvents` to []

**Effort**: 1 hour.

### ☐ T2.5 — Conversation title (auto-generated summary)

**Pattern observed**: The conversation has a title at the top
("先step4"). Looks user-set or auto-summarized from first prompt.

**Why**: with multiple Claude sessions per Room (T2.3 + T2.4),
titles help navigation.

**How**:
- v0: just use the first user prompt's first 30 chars
- v1: ask Claude for a 5-word summary on session close (call SDK
  with a quick "summarize this conversation in ≤5 words" prompt
  pointed at the transcript)

**Effort**: 1–2 hours for v0; 4 hours for v1.

## Tier 3 — polish / nice-to-have

### ☐ T3.1 — File-reference chips in user prompts

**Pattern observed**: Files mentioned in the prompt appear as small
clickable chips ("📄 CLAUDE.md") that open the file in the editor
when clicked.

**Why**: the prompt is about a file → user wants to open it. One
click instead of Cmd-click navigation.

**How**:
- After the user submits a prompt, scan it for `path/to/file`
  patterns (file extensions or paths starting with `./` `../`)
- Replace with a chip component
- Click → `vscode.workspace.openTextDocument(path)` +
  `window.showTextDocument`

**Effort**: 2 hours.

### ☐ T3.2 — Tool call IN/OUT layout (vs current single card)

**Pattern observed**: Tool calls render as two stacked blocks
labeled `IN` (the input/command) and `OUT` (the result), with a
clear visual separator.

**Why**: makes it more obvious what was input vs what came back.

**How**:
- In `Claude.tsx::ToolCard`, restructure: top half is
  `IN: <name>(<input summary>)`, bottom half is `OUT: <result>`
- Style as two side-by-side or stacked rows with subtle dividers

**Effort**: 1 hour.

### ☐ T3.3 — Step state colors (●/○/✗ with semantic colors)

**Pattern observed**: Bullets are colored:
- gray hollow ○ = pending
- green filled ● = success
- red filled ● with × = failed

**Why**: at a glance, where did the turn fail / how far did it
get.

**How**: ties into T1.1's bullet rendering. Use VSCode theme
tokens: `charts.green` / `charts.red` / `disabledForeground`.

**Effort**: rolled into T1.1.

### ☐ T3.4 — Voice input

**Pattern observed**: Microphone icon in the input.

**Why**: matches mainstream IM. But: requires browser
`MediaRecorder` API + a transcription service. Not feasible in v0
without an extra dep + API key.

**Decision**: skip for now.

## Out of scope (for this plan)

These were also visible in the official extension but are outside
cc-connect's scope or are already covered:

- Conversation tabs / multi-window — VSCode users can do this via
  `vscode.workspace.openTextDocument` patterns; not our pane's job
- "Edit Claude's plans before accepting them" (plan mode) — would
  ride on T1.3's `permissionMode: 'plan'` selection; the doc covers
  it indirectly
- Auto-accept edits — same as T1.3 with `permissionMode:
  'acceptEdits'`
- @-mention files with line ranges — could combine T3.1 with
  Cmd-click selection; deferred
- "Open multiple conversations in separate tabs or windows" —
  cc-connect's surface is single-pane; could expose a
  "duplicate Room view" command later

## Progress (2026-05-05 batch)

7 of 13 items shipped this session:

- ☑ T1.1 step timeline · ☑ T1.2 stop · ☑ T1.4 queue pill ·
  ☑ T1.5 thinking
- ☑ T2.1 slash · ☑ T2.2 attach · ☑ T2.4 new chat

Remaining (in priority order from below):

- ☐ T1.3 permission UI (deferred — biggest single feature; the
  inline approval bubble + canUseTool path is non-trivial under
  the headless ZodError constraint)
- ☐ T2.3 history · ☐ T2.5 titles (paired — title display only
  becomes useful once history exists)
- ☐ T3.1 file-ref chips · ☐ T3.2 IN/OUT layout · ☐ T3.3 already
  rolled into T1.1

## Suggested implementation order

For maximum visible improvement per session-hour:

1. **T1.4** (queue placeholder) — 30 min, immediate UX win
2. **T1.2** (Stop button) — 1–2 hours, addresses a real "stuck Claude" pain
3. **T1.5** (Thinking indicator) — 1–2 hours, reduces "is it doing anything" anxiety
4. **T1.1** (Step timeline) — 1–2 hours, the biggest visual upgrade
5. **T2.1** (Slash launcher) + **T2.2** (Attach button) — 3 hours together; do in one PR
6. **T1.3** (Permission UI with inline approval) — 4–6 hours, biggest single feature; saves for last in this batch
7. **T2.3** + **T2.4** + **T2.5** (history / new chat / titles) — 6 hours together; coherent feature set
8. **T3.x** — opportunistic polish

## Working agreement

- **Don't lift code** from the official extension. Patterns are
  observable; specific TS/React code is proprietary.
- **Don't read** `~/work/claude-code-main` or any reconstructed-
  from-source-map repos. cc-connect is MIT/Apache; mixing in
  leaked source taints contributors.
- **Reference sugyan/claude-code-webui** (MIT, 1.1k★) when stuck
  on similar problems — that's the OSS analog.
- Each item gets a commit per design doc convention
  (`feat(vscode-extension): …`).
