---
name: push
description: Push local commits to origin/main with the right pre-flight checks — local compile, conventional-commits guard, branch protection awareness. Use when the user says "push", "push this", "push to main", or any short instruction to land local work on the remote. NOT for cutting releases — those go through the `publish` skill, not here.
---

# Pushing to origin

Goal: get local commits to `origin/main` cleanly, **without surprising the user** with a hook failure mid-push or accidentally bypassing the wrong safety net.

This skill handles **just pushing branch heads**. Tags, releases, version bumps → use the `publish` skill instead. The two are deliberately separate so "push my work" doesn't accidentally cut a release.

## Quick path

```bash
# 1. Status check — show me what's about to land
git status -sb
git log --oneline @{upstream}..HEAD

# 2. Pre-flight (see "Pre-flight checks" below for the matrix)
#    Default: don't skip these. They're cheap relative to a CI fail.

# 3. Push
git push origin <branch>
```

That's the happy path. The rest of this doc covers the things that go wrong.

## Pre-flight checks

Run the relevant subset based on what the diff touches. **Do not skip "the test suite is slow" — run an incremental subset, not nothing.**

| What changed | Run before push |
|---|---|
| `vscode-extension/**/*.ts(x)` | `cd vscode-extension && bun run compile && bunx tsc -p tsconfig.webview.json` |
| `crates/**/*.rs` | `cargo check --workspace` (full `cargo build --release` only if you suspect a release-only path; `check` catches 90 % of breaks in 10 % of the time) |
| `chat-ui/**/*.{ts,tsx}` | `cd chat-ui && bunx tsc --noEmit` |
| `install.sh` / `scripts/**.sh` | `bash -n install.sh && bash -n scripts/<changed>.sh` (syntax check) |
| `.github/workflows/**.yml` | Spot-check matchers, especially the `tags:` glob. Anchor `v[0-9]*.*.*` not `v*.*.*` to avoid extension-tag collision. |
| `layouts/*.md` | Both halves consume these — see `vscode-extension/CLAUDE.md` "Launcher-parity prompts". Re-run the `vscode-extension` compile so the copy-into-`dist/layouts/` step picks up the change. |
| `Cargo.lock`, `chat-ui/bun.lock`, `vscode-extension/bun.lock` | All three are tracked on purpose (release CI uses `--frozen-lockfile`). Don't gitignore them; **do** commit changes. |

If type-checks or compile fail → fix first, **don't push the broken state**. Conventional-commits hook + branch-protection check happen on push, but a broken compile that lands on `main` will block every other contributor's CI.

## Conventional Commits guard

The repo's commit-msg hook enforces:

```
<type>(<scope>)?: <subject>
  type    feat | fix | chore | docs | refactor | test | perf | build | ci | revert
  scope   optional, lowercase, comma-separated for multi-area changes
  subject imperative mood, lower-case, no trailing period, ≤72 chars
```

Common rejections + fixes:

| Reject reason | Fix |
|---|---|
| Subject > 72 chars | Move detail into the body. Subject is the headline, body is the why. |
| Wrong type (`feature` instead of `feat`, `bugfix` instead of `fix`) | Use one of the allowed types. The repo doesn't accept aliases. |
| `Add` / `Added` instead of `add` | Imperative + lowercase only. |
| Trailing period in subject | Remove it. |
| No scope | Scope is optional but **strongly preferred** — the existing log uses scopes for every commit. Pick a real component name (`hook`, `mcp`, `tui`, `chat-ui`, `chat-daemon`, `vscode-extension`, `install`, `room`, `security`, `ci`, …). |

If the hook rejects, **don't `--no-verify`**. Edit the message:

```bash
git commit --amend
# Or for the most recent N commits:
git rebase -i HEAD~N    # mark each as "reword"
```

`--no-verify` is reserved for genuine emergency surgical fixes (e.g. when the hook itself is broken). Push that follows almost always shouldn't bypass either.

## Branch protection

`main` has branch protection: PRs are required, 6 status checks must pass. The user's account has bypass permissions, so direct `git push origin main` succeeds but produces these warnings:

```
remote: Bypassed rule violations for refs/heads/main:
remote:   - Changes must be made through a pull request.
remote:   - 6 of 6 required status checks are expected.
```

**This is normal for the user's account.** Do not interpret it as an error. If you see it, the push succeeded.

If pushing on behalf of someone without bypass permissions: open a PR instead. The `gh` CLI handles this:

```bash
git push origin -u <feature-branch>
gh pr create --base main --fill
```

## Pushing while behind origin

If `git status -sb` shows `[behind N]`, pull first. Default to `--ff-only`:

```bash
git pull --ff-only origin main
```

If that fails (you have local commits that diverged from remote), **rebase, do not merge**. The repo style is linear history:

```bash
git pull --rebase origin main
# Resolve conflicts → git rebase --continue
git push origin main
```

If the rebase pulls in a merge commit, abort and ask the user how they'd prefer to handle it (merge commits on `main` are rare here and usually deliberate).

## Pushing a feature branch (not `main`)

If `git branch --show-current` is not `main`:

```bash
git push origin -u <branch>      # first push: -u to set upstream
git push origin                  # subsequent pushes: just push
```

Don't push to `main` from a feature branch — the user wants `main` to track work-in-progress on `main` only. If they say "push" while on a feature branch, push **the feature branch**, not `main`.

## Watching CI after push

```bash
gh run list --limit 3            # see what just kicked off
gh run watch --exit-status       # block until the most recent run finishes
```

The repo's `ci.yml` runs on every `main` push (Rust + chat-ui + vscode-extension type-check). It usually takes 1-2 min. If you push something that breaks it, fix-forward (a follow-up commit) is cleaner than `git revert`.

## Things this skill does NOT do

- **Cut a release / push a tag** → use the `publish` skill. Tag pushes trigger the release workflows; mixing release work into a routine push almost always means the user forgot a step.
- **Push to remote without a local commit** → if the working tree has uncommitted changes, ask the user whether to commit them first or stash. Don't auto-commit unless they explicitly said "commit and push".
- **`git push --force` without explicit user authorisation** → force-pushing main is destructive. The Bash tool's safety rules forbid it; this skill defers to that. For a force-with-lease on a feature branch, ask first.

## When in doubt

`git status -sb && git log --oneline @{upstream}..HEAD` shows exactly what's about to be pushed. Read it back to the user before pushing if the diff is large or the branch is unusual. The 5-second check has caught at least one accidental "push of stale rebase work".
