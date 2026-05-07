# TODOS

## WORKAROUND ACTIVE: vendored ed25519 + ed25519-dalek (pending upstream PR)

**Status:** Resolved locally via `[patch.crates-io]` pointing at vendored copies of `ed25519 3.0.0-rc.4` and `ed25519-dalek 3.0.0-pre.1` with three+two-line fixes. cc-connect-core, cc-connect bin (`host` / `doctor` / placeholder `chat`), and cc-connect-hook bin all build and test on stable Rust 1.95.

**Root cause** (for the historical record): `pkcs8::Error::KeyMalformed` was changed from a unit variant to a tuple variant `KeyMalformed(KeyError)`. Both `ed25519 3.0.0-rc.4` and `ed25519-dalek 3.0.0-pre.1` still reference it as a unit variant. Bare `Error::KeyMalformed` is therefore a `fn(KeyError) -> Error` function pointer, not an `Error` value, so `?` and `return Err(...)` both fail to type-check.

**Local fix in this repo:**
- `vendored/ed25519/src/pkcs8.rs` — three sites (lines 172, 173, 179) updated to `Error::KeyMalformed(KeyError::Invalid)` plus a `KeyError` import.
- `vendored/ed25519-dalek/src/signing.rs` — two sites (lines 714, 717) updated similarly.
- Workspace `Cargo.toml`: `[patch.crates-io] ed25519 = { path = "vendored/ed25519" }`, `ed25519-dalek = { path = "vendored/ed25519-dalek" }`.

**Upstream PRs / issues filed (2026-04-28):**
- `n0-computer/iroh#4192` — comment with full root-cause + workaround diff. Closed 2026-04-28.
- `RustCrypto/signatures#1315` — root-cause issue. Closed 2026-04-28.
- `dalek-cryptography/curve25519-dalek#901` — PR with the semantic fix. Closed in favour of `#902` ("bump rustcrypto dependencies to released versions"), merged 2026-05-02.

**Upstream status as of 2026-05-07:**
- `ed25519` 3.0.0 stable released 2026-05-03 (and 3.0.0-rc.5 on 2026-04-28).
- `ed25519-dalek` 3.0.0-pre.7 released 2026-05-06 — this is the first published version that compiles against current pkcs8.
- `iroh 0.97.0` exact-pins `ed25519-dalek =3.0.0-pre.1`. `iroh 0.98.x` (latest 0.98.2) bumps the pin to `=3.0.0-pre.6`. Both pre-fix; **the iroh stack still has not released a version that picks up `pre.7+`**.
- Verified locally on 2026-05-07: removing `[patch.crates-io]` and re-running `cargo update -p ed25519-dalek` resolves to `pre.1` (constrained by iroh) and `cargo check` fails on the same `Err(pkcs8::Error::KeyMalformed)` callsites.

**Removal trigger:** when iroh ships a release that pins `ed25519-dalek 3.0.0-pre.7` or later. Watch n0-computer/iroh release notes; the cc-connect side is one-line ready (the patch entries can come out the same commit that bumps the iroh deps). At that point:
1. Bump `iroh` / `iroh-blobs` / `iroh-gossip` together (their cross-pins move in lockstep).
2. `cargo update` to pull the new ed25519-dalek transitively.
3. Delete `vendored/ed25519/` and `vendored/ed25519-dalek/`.
4. Remove the two `[patch.crates-io]` entries in workspace `Cargo.toml`.
5. Verify `cargo test --workspace` and `cc-connect host` still work.
6. Commit "chore: drop vendored ed25519 patches now that iroh ships pre.7+."

---

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

---

## v0.3+

### Cross-AI adapter framework (codex / gemini-cli / aider / cursor-cli)

**What:** Generalise the `cc-connect-tui` PTY embedding so the right pane can run any interactive AI CLI, not just Claude Code. Inject `cc-connect-hook` (or equivalent) output as pre-prompt context for AIs that lack a native pre-prompt hook.

**Why:** Mixed-AI rooms — one peer's Claude, another peer's Codex, a third peer's aider — should be able to share the same chat substrate. The chat protocol itself is AI-agnostic; only the context-injection mechanism is Claude-specific.

**Design sketch:**
- Add `--ai <bin>` to `cc-connect-tui start|join` (default `claude`).
- Add `--prompt-decorator <cmd>` for AIs without native pre-prompt hooks. The TUI runs `<cmd>` (e.g. `cc-connect-hook`) right before forwarding each user prompt to the PTY child, prepending its stdout to the user's input bytes.
- Per-AI: detect "user has hit Enter to submit a prompt". For TUI-style AIs (claude, codex), this is the Enter key when the input box is non-empty. For REPL-style (aider), it's the line being submitted.
- Compatibility table: keep this matrix in README:
    - `claude` — uses native UserPromptSubmit hook, `--prompt-decorator` ignored
    - `aider` — `--prompt-decorator cc-connect-hook` works (REPL prompt detection trivial)
    - `codex` — needs PTY-level prompt detection; ~1-2h tuning
    - `gemini-cli` — same as codex
    - `cursor-cli` — low priority (CLI is a wrapper around the GUI)

**Pros:** Multi-AI parity. Existing infra (chat substrate, PTY embedding) does most of the heavy lifting.

**Cons:** Per-AI prompt-detection is fragile — every AI release might shift it. Probably need `--prompt-detector <regex>` escape hatch.

**Context:** User asked during v0.2 review (2026-04-29). Explicitly deferred from v0.2 to keep the @ + nick + Ctrl-C work focused; v0.3 candidate.

**Depends on:** v0.2 stabilises.
