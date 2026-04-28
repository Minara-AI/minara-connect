# TODOS

## v0.1 implementation

### Bootstrap race UX

**What:** `cc-connect chat <ticket>` must not accept user input until gossip is joined and Backfill is complete (or has timed out).

**Why:** If the user types a question to Claude before bootstrap finishes, the Hook fires with no active Room and silently injects nothing. The user blames cc-connect.

**Pros:** Eliminates the most embarrassing first-run failure mode. Trivial: print `Connecting…` then `Joined.` and gate the readline on a ready-flag.

**Cons:** None.

**Context:** Identified during /plan-eng-review (Section 1, Architecture observation #1). Tied to ADR-0003 (PID file is written at end of bootstrap) but the user-facing message is missing.

**Depends on:** none.

---

## v0.2

### Skill packaging investigation

**What:** Spike a Claude Code skill named `/cc-connect-join` that auto-installs the hook entry into `~/.claude/settings.json` and starts a `cc-connect chat` process in the user's tmux pane.

**Why:** Manual settings.json snippet is the v0.1 install UX. It's brittle (path issues, JSON merge errors). A skill is one slash command and done.

**Pros:** Clean install. Demonstrates cc-connect as a Claude Code-native artifact, not just an external CLI.

**Cons:** Skill ecosystem is still moving in 2026; v0.2 timing matters. Adding a skill adds the v0.1 → v0.2 surface area.

**Context:** Original design doc Open Question 6.

**Depends on:** v0.1 ships first.

---

### Cursor format extension: byte_offset for log scan

**What:** Cursor file stores `{ulid, byte_offset}` instead of just `ulid`. Hook seeks to byte_offset, verifies the next record's ULID matches, then continues forward.

**Why:** As log.jsonl grows (heavy daily users could see 100k+ messages over months), reading "since cursor" via linear scan becomes O(N). With byte offset, it's O(unread).

**Pros:** Performance ceiling raised. ~20 lines of code. Cursor format change is additive (clients that ignore byte_offset still work).

**Cons:** Small protocol surface increase.

**Context:** Performance review Section 4. Explicitly deferred from v0.1 to keep scope tight; flag for inclusion in v0.2 (or v0.1 if scope room appears).

**Depends on:** PROTOCOL.md cursor section.
