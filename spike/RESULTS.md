# Spike 0 Results

Run on: __FILL IN DATE__
Claude Code version: __FILL IN (run `claude --version`)__
Platform: __FILL IN (e.g. macOS 14.5 arm64)__

---

## Trial matrix

| Size | BEGIN sentinel seen verbatim? | END sentinel seen verbatim? | Lowest POS visible | Highest POS visible | Notes |
|---|---|---|---|---|---|
| 1 KB | _ / _ | _ / _ | _ | _ | |
| 8 KB | _ / _ | _ / _ | _ | _ | |
| 64 KB | _ / _ | _ / _ | _ | _ | |
| 256 KB | _ / _ | _ / _ | _ | _ | |

(`expected_lines` for reference: 1KB→12, 8KB→102, 64KB→819, 256KB→3276)

---

## Observed cap

The largest size at which the END sentinel is preserved verbatim: **__ KB**.

If a size is truncated in the middle: the highest visible `POS-NNNNN` is **POS-_____**, which corresponds to roughly **___ bytes** (POS × 80).

If overflow caused a hard error rather than silent truncation: paste the error here:

```
__paste any error / refusal here__
```

---

## Decision

Pick the line that matches the trial:

- [ ] **All sizes ≤ 8 KB pass cleanly.** Design's 8 KB hook budget is safe. Proceed to Week 1 (`PROTOCOL.md` draft) on the existing plan.
- [ ] **8 KB passes but 64 KB / 256 KB silently truncates.** Document the observed cap in `PROTOCOL.md` and keep the 8 KB budget as a safety margin.
- [ ] **8 KB passes but 64 KB / 256 KB causes hard refusal or crash.** Lower the hook budget to a comfortable margin (e.g. 4 KB) and harden `cc-connect doctor` to warn if a single chat Message would exceed it.
- [ ] **8 KB silently truncates.** Lower hook budget to whatever passed cleanly, update PROTOCOL.md, proceed.
- [ ] **Even 1 KB doesn't appear in Claude's context.** Trigger **Plan B**: hook contract is unsuitable. Switch v0.1 plan to MCP-server-as-context-source. Stop; reopen the design doc to revise.

---

## Verbatim Claude output (optional but useful)

Paste each trial's full Claude response here for the historical record. This is the spike's primary evidence and should not be summarised away.

### 1 KB

```
__paste verbatim__
```

### 8 KB

```
__paste verbatim__
```

### 64 KB

```
__paste verbatim__
```

### 256 KB

```
__paste verbatim__
```
