# Spike 0 — UserPromptSubmit byte-cap probe

**Goal:** Find Claude Code's per-prompt `UserPromptSubmit` stdout injection cap (and its overflow behaviour) before any cc-connect Rust code is written. The result determines whether the design's 8 KB hook budget is correct, or whether the hook contract needs a fundamental rework.

**Status:** This spike is a **Day-0 BLOCKING** task in the cc-connect v0.1 plan (see `~/.gstack/projects/cc-connect/yijian-main-design-*.md`). Every downstream decision — protocol shape, cursor format, fcntl semantics — assumes the hook contract works as designed.

---

## Files

- `userpromptsubmit-spike.sh` — emits a known-pattern blob of N KB to stdout. Each line is exactly 80 bytes (incl. newline) and tagged `POS-NNNNN`. Begin/end sentinels bound the blob.
- `RESULTS.md` — template for recording observations and the resulting decision.

---

## Procedure

You'll need a **fresh Claude Code session** (settings.json is loaded at start). The procedure below uses a temporary global hook entry. Restore your settings.json when done — leaving the spike hook installed will inject ~256 KB into every prompt.

### 1. Back up settings.json

```bash
cp ~/.claude/settings.json ~/.claude/settings.json.bak-spike
```

### 2. Add the temporary hook entry

In `~/.claude/settings.json`, add (or extend) the `hooks` block:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "type": "command",
        "command": "CC_SPIKE_SIZE_KB=1 /Users/yijian/work/cc-connect/spike/userpromptsubmit-spike.sh"
      }
    ]
  }
}
```

You'll re-edit the `CC_SPIKE_SIZE_KB` value four times: 1, 8, 64, 256.

### 3. Run the four trials

For each `SIZE` in {1, 8, 64, 256}:

1. Set `CC_SPIKE_SIZE_KB=$SIZE` in the hook entry above.
2. **Open a fresh Claude Code session** (the hook config is read at session start).
3. Issue this prompt verbatim:

   > A hook just injected content into your context. Tell me:
   > 1. The literal text of the BEGIN sentinel line.
   > 2. The literal text of the END sentinel line.
   > 3. The lowest `POS-NNNNN` number you can see.
   > 4. The highest `POS-NNNNN` number you can see.
   > 5. Whether you see ANY content matching `POS-NNNNN` at all.

4. Record Claude's answers in `RESULTS.md` under the matching size column.

### 4. Restore settings.json

```bash
mv ~/.claude/settings.json.bak-spike ~/.claude/settings.json
```

Open a fresh Claude Code session to confirm the spike hook is gone.

---

## What the results tell us

| Observation | Meaning | Action for v0.1 |
|---|---|---|
| 1 KB fully visible, lower POS = 1, higher POS = 12 | hook stdout works as advertised at small sizes | continue |
| 8 KB fully visible | the design's 8 KB budget is fine | continue, document budget |
| 64 KB fully visible | hook can carry more than the design assumes — relax 8 KB to (say) 32 KB | continue, raise budget |
| 64 KB or 256 KB **truncates silently** at some point in the middle | there's a cap; record where | bake the observed cap into the hook contract; verify drop-oldest formatting still works |
| 64 KB or 256 KB causes Claude Code to **reject the prompt or crash** | hook output is bounded by something stricter than truncation | reduce hook budget AND surface a `cc-connect doctor` warning |
| Even 1 KB doesn't show up | the hook contract is fundamentally different from what we assumed | trigger Plan B: switch v0.1 to MCP-server-as-context-source (adds ~2 weeks; design doc already documents this fallback) |

The design doc's Open Question 1 is the source of truth on Plan B.

---

## After the spike

1. Fill in `RESULTS.md` with what Claude reported in each trial.
2. Note the smallest size that shows truncation (if any) and the largest size that shows full content.
3. If results match assumptions: write a one-paragraph note in `RESULTS.md` saying "design budget of 8 KB is safe" and proceed to Week 1 (`PROTOCOL.md` draft).
4. If results trigger Plan B: stop, return to the design doc, revise hook contract → MCP, then resume.

The spike is throwaway. After RESULTS.md is filled in, this entire `spike/` directory can stay in the repo as historical evidence (recommended) or be deleted. Either is fine.
