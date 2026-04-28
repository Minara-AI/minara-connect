# Hook payload budget is 8 KB, with Claude Code's persistence as a graceful overflow safety net

## Decision

The cc-connect Hook (`cc-connect-hook`) emits at most **8 KB** to stdout per `UserPromptSubmit` invocation. Within that budget, it formats the unread Messages chronologically (oldest first within the kept window) and drops *older* Messages from the emission if the formatted output would exceed the budget. A `[chatroom] (N older messages skipped to fit)` marker is prepended whenever the drop occurred.

If a future bug causes the hook to exceed 8 KB anyway, **no data is lost**: Claude Code persists the full stdout to `~/.claude/projects/<project>/<session>/tool-results/hook-<uuid>-stdout.txt` and injects a ~2 KB inline preview plus a `<persisted-output>` system-reminder. Claude can `Read` the persisted file on demand.

## Spike 0 evidence

Spike 0 (see `spike/RESULTS.md`, commit 137e031) probed Claude Code's `UserPromptSubmit` stdout handling at 1 / 8 / 64 / 256 KB:

| Size | Inline behaviour | Persisted file? |
|---|---|---|
| 1 KB | Full payload inline (BEGIN, all 12 POS lines, END) | n/a |
| 8 KB | Full payload inline (BEGIN, all 102 POS lines, END) | n/a |
| 64 KB | Inline preview only (BEGIN + first ~24 POS lines ≈ 2 KB), `<persisted-output>` system-reminder injected | full 64 KB persisted, all 819 POS lines + END intact |
| 256 KB | Same pattern as 64 KB; inline preview cuts off at POS-00024 | full 256 KB persisted, all 3276 POS lines + END intact |

The transition between "all inline" and "preview + persisted" happens somewhere between 8 KB and 64 KB. The exact threshold is undocumented but irrelevant: Claude Code's behaviour is *graceful* — overflow is announced via the system-reminder, not silently truncated, and the full payload is recoverable.

## Why 8 KB is the right hook budget

- **Verified safe inline** — 8 KB passed the spike with full BEGIN/END sentinels and every POS line. The design doc's original 8 KB budget assumption holds.
- **Above 8 KB the inline preview drops to ~2 KB** — Claude inline-sees only the first ~24 lines (≈2 KB) regardless of how much we emit. Emitting 32 KB or 64 KB gives Claude *less* useful inline context than emitting a tight 8 KB.
- **Common cc-connect usage stays well under 8 KB** — typical pair-prog has 0-5 unread Messages (~500 B). 50 unread (15-30 minutes of active chat) is ~5 KB. The 8 KB ceiling is generous.
- **Overflow is graceful, not catastrophic** — even if a v0.1 bug or pathological edge case (a single 9 KB Message body) crosses 8 KB, Claude Code's persistence path means no data loss and a discoverable overflow signal. We don't need to defend against this aggressively.

## Implications for the v0.1 plan

- **Plan B (MCP-server-as-context-source) is no longer a v0.1 fallback.** The hook contract works as the design assumed at the 8 KB scale we need. MCP becomes a v0.2 enhancement (richer access patterns, on-demand queries) rather than an emergency replacement.
- **Hook contract in PROTOCOL.md** keeps the design's "cap at 8 KB, drop oldest, emit chronologically" rule. No revision required — Spike 0 confirmed it.
- **Open Question 1** in the design doc is closed.
- **`cc-connect doctor`** should verify hook stdout works at the 8 KB scale (a small smoke test that emits 7 KB and asks the user "did Claude see all of this?"), but does *not* need to defend against the persistence path.

## Trade-off acknowledged

The 8 KB cap means a sustained burst of activity (200+ unread Messages while Alice's Claude is silent) will drop the oldest messages from a single injection. Those messages remain in Alice's local `log.jsonl`; she can scroll them in her chat pane. Her Claude doesn't see them inline. For "ambient awareness" framing this is acceptable: ambient means "stay current," not "catch up on hours of backlog." Users who want the catch-up affordance will get it in v0.2 via MCP-server queries.
