# Spike 0 Runbook — for a fresh Claude Code session to drive

Paste this entire file into a **fresh** Claude Code session as your first message. The session will: (1) verify the spike hook is installed, (2) inspect its own injected context, (3) record findings to `RESULTS.md`, (4) advance `CC_SPIKE_SIZE_KB` to the next value, (5) tell you to restart and paste this file again. After the four trials it restores your `settings.json` and commits the results.

> **Why a fresh session?** Claude Code reads `~/.claude/settings.json` at session startup. Each trial size needs its own startup. There is no way for a single session to run multiple trials.

---

## ONE-TIME PREP (the human does this once, before the first session)

```bash
# 1) Back up settings.json (the runbook will refuse to overwrite)
test -f ~/.claude/settings.json.bak-spike && \
  echo "ABORT: backup exists; restore or remove it before starting a new spike" || \
  cp ~/.claude/settings.json ~/.claude/settings.json.bak-spike

# 2) Install the spike hook at SIZE=1 KB
#    Use the exact JSON below. Merge with any existing "hooks" section in settings.json.
#    If your settings.json has no "hooks" key, add the entire block.
```

Add or merge into `~/.claude/settings.json`:

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

```bash
# 3) Open a NEW Claude Code session (close any existing one).
# 4) Paste this file's contents as the first message in that new session.
```

---

## INSTRUCTIONS FOR THE FRESH CLAUDE CODE SESSION

You are a fresh Claude Code session that just had a `UserPromptSubmit` hook fire. Your job is to execute the next step of cc-connect's Spike 0 protocol.

### Step 1 — verify the environment

Run these commands in order. **Stop and report the failing step if anything errors.**

```bash
cd /Users/yijian/work/cc-connect
test -f spike/RESULTS.md
test -f spike/userpromptsubmit-spike.sh
test -f ~/.claude/settings.json.bak-spike || echo "WARNING: no backup at ~/.claude/settings.json.bak-spike — do not proceed"
```

Read `~/.claude/settings.json` and locate the `UserPromptSubmit` hook entry that runs `userpromptsubmit-spike.sh`. Extract the current `CC_SPIKE_SIZE_KB` value from the `command` string. This is your **CURRENT_SIZE**.

If the hook entry is not present, stop and tell the human: "spike hook is not installed in settings.json; redo the prep step."

### Step 2 — verify the hook actually fired in this session

In your context window from this turn, look for the literal string `<<<SPIKE-BEGIN`. The hook prepends a blob to your prompt that begins with this sentinel.

- If you can see `<<<SPIKE-BEGIN size_kb=N expected_lines=M expected_bytes=B>>>`: confirm `N == CURRENT_SIZE`. Capture `M` (expected_lines) and `B` (expected_bytes).
- If you cannot see the BEGIN sentinel: hook output is being dropped or the hook didn't run. Skip to Step 5 (Plan B trigger).

### Step 3 — inspect your context and record observations

Look through your prompt context for the spike output. Record these answers verbatim:

1. **BEGIN sentinel** — copy the BEGIN line literally. Did you see it? Y/N. Full text:
2. **END sentinel** — search for `<<<SPIKE-END last_line=POS-NNNNN>>>`. Did you see it? Y/N. Full text:
3. **Lowest visible POS** — what's the smallest `POS-NNNNN` number you can see? (Should be `POS-00001` if no truncation at the start.)
4. **Highest visible POS** — what's the largest `POS-NNNNN` number you can see? (Should equal `expected_lines` from BEGIN if no truncation.)
5. **Mid-truncation evidence** — are any consecutive POS numbers missing in the middle?

Compute: did you see the full payload? `Y` if BEGIN + END both seen verbatim AND highest POS == expected_lines AND no mid-gaps. Otherwise `N`.

Edit `spike/RESULTS.md`. Find the row for `CURRENT_SIZE` KB in the trial matrix table and fill it in. Also paste the verbatim text of the BEGIN sentinel, the END sentinel (or note its absence), and a sample of the highest-POS lines you can see in the corresponding `### N KB` section near the bottom.

### Step 4 — advance to the next trial OR finish

The trial sequence is: 1 → 8 → 64 → 256 → DONE.

#### If CURRENT_SIZE is 1, 8, or 64:

Edit `~/.claude/settings.json` and change `CC_SPIKE_SIZE_KB=<CURRENT>` to `CC_SPIKE_SIZE_KB=<NEXT>` in the hook command (1→8, 8→64, 64→256). Save.

Then tell the human verbatim:

> "Trial CURRENT_SIZE recorded. Now please:
> 1. Close this Claude Code session.
> 2. Open a new Claude Code session.
> 3. Paste `spike/RUNBOOK.md` as the first message again.
>
> The next trial will run at NEXT KB."

Stop. Do not do anything else.

#### If CURRENT_SIZE is 256 (final trial):

You have all four trials. Now:

1. Read `spike/RESULTS.md`. Verify all four rows of the trial matrix are filled in.
2. Look at the "## Decision" section. Pick the one checkbox that matches the observed pattern across the four trials. Replace `[ ]` with `[x]` for the matching line, and a one-paragraph rationale below it referring to the specific evidence (e.g. "8 KB row shows END sentinel preserved verbatim; 64 KB row shows highest POS = POS-00510, which is ~40 KB, indicating a cap somewhere between 8 KB and 64 KB").
3. Restore the human's settings.json:
   ```bash
   mv ~/.claude/settings.json.bak-spike ~/.claude/settings.json
   ```
4. Commit:
   ```bash
   git add spike/RESULTS.md
   git commit -m "$(cat <<'EOF'
   docs(spike): record Spike 0 results across 1/8/64/256 KB trials

   See spike/RESULTS.md for the verbatim Claude responses and the
   chosen decision branch. This unblocks Week 1 (PROTOCOL.md draft).

   Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
   EOF
   )"
   ```
5. Print to the human:
   - The chosen decision branch.
   - The implication for the v0.1 plan: "proceed on existing plan" or "trigger Plan B (switch to MCP-server-as-context-source)".
   - The next step: "open a new Claude Code session and run `/plan-eng-review` if Plan B was triggered, OR start drafting `PROTOCOL.md` per design doc Week 1."

### Step 5 — Plan B trigger (only if Step 2 found no BEGIN sentinel)

If even the 1 KB trial shows no `<<<SPIKE-BEGIN` in your context, the hook output is not reaching Claude's context the way the design assumed.

1. Edit `spike/RESULTS.md`. In the trial matrix, mark `CURRENT_SIZE` row as "BEGIN: N — hook stdout not visible in context".
2. In the Decision section, check the box for **"Even 1 KB doesn't appear in Claude's context"**.
3. Tell the human:

> "Plan B triggered. Hook stdout injection is not behaving as the design assumed. Stop the trial sequence — running 8 / 64 / 256 will tell us nothing more.
>
> Restore your settings.json:
>     mv ~/.claude/settings.json.bak-spike ~/.claude/settings.json
>
> Then open a new Claude Code session and report this finding to the cc-connect design conversation. The design doc's Open Question 1 ('hook output cap spike') has Plan B documented: switch v0.1 from `UserPromptSubmit` injection to an MCP server exposing `cc://room/<id>/recent` as a resource. Adds ~2 weeks to v0.1.
>
> Do NOT advance settings.json to the next size. Do NOT commit. The design doc needs revision first."

Stop.

---

## Safety notes (for the agent)

- You have permission to edit `~/.claude/settings.json` and `spike/RESULTS.md`. Both edits are reversible: settings.json has a backup, RESULTS.md is git-tracked.
- You have permission to run `mv ~/.claude/settings.json.bak-spike ~/.claude/settings.json` ONLY in the final-trial path (Step 4 256 KB) or the Plan B path (Step 5).
- Do **not** attempt to "test the hook" by issuing prompts. You ARE the test. Your context is the evidence.
- Do **not** run `cc-connect` Rust code. None exists yet — that's the whole point of this spike.
- If anything is ambiguous, stop and ask the human.

---

## Sanity reference

The spike script's per-line format: `POS-NNNNN-XXXXX...` where the X-padding makes each line exactly 80 bytes including the trailing newline. Expected line counts:

| SIZE | expected_lines | expected_bytes |
|---|---|---|
| 1 KB | 12 | 1024 (1062 actual incl. sentinels) |
| 8 KB | 102 | 8192 (8263 actual) |
| 64 KB | 819 | 65536 (65625 actual) |
| 256 KB | 3276 | 262144 (262188 actual) |

If you see fewer POS lines than expected_lines for a given trial, that's where Claude Code's cap is.
