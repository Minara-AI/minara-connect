---
name: publish
description: Cut a release of cc-connect — Rust binaries (`v*` tag), VSCode extension (`vscode-extension-v*` tag), or both. Use when the user says "publish", "release", "cut a release", "tag a release", "ship", or anything that implies pushing a versioned artifact. Handles version bumps, tag namespaces, CI watch, and the cross-artifact dependencies users keep forgetting.
---

# Publishing cc-connect

This repo ships **two independent artifacts** with their own release cadences. Pick the right one for the change being released; if the change touches both halves, release **both** in the right order.

## The two artifacts

| Artifact | Tag pattern | CI workflow | What it ships |
|---|---|---|---|
| **Rust binaries** | `v[0-9]*.*.*` (e.g. `v0.5.0-alpha`, `v1.0.0`) | [`.github/workflows/release.yml`](../../../.github/workflows/release.yml) | Per-platform tarballs of `cc-connect`, `cc-connect-tui`, `cc-connect-hook`, `cc-connect-mcp`, `cc-chat-ui` plus `install.sh`. Attached to GitHub Release. |
| **VSCode extension** | `vscode-extension-v*.*.*` | [`.github/workflows/vscode-extension-release.yml`](../../../.github/workflows/vscode-extension-release.yml) | `cc-connect-vscode-<version>.vsix` attached to GitHub Release. |

The workflows are independent — tagging one does **not** trigger the other.

## Decide what to release

Ask the user, or infer from `git log --oneline <last-tag>..HEAD`:

- **Rust-only** — changes touched any of: `crates/`, `install.sh`, `scripts/`, `layouts/*.md`, `chat-ui/`, `Cargo.toml`, `release.yml`. Tag `v*`.
- **VSCode-only** — changes are confined to `vscode-extension/`. Tag `vscode-extension-v*`.
- **Both** — common when a Rust-side feature needs an extension UI to be useful (e.g. layout-prompt changes affect both `claude-wrap.sh` and `vscode-extension/src/host/prompts.ts`). Release **Rust first**, then the extension that depends on it.

If unsure, default to the more conservative answer: **release the Rust side first**, then check whether anything in the extension needs the new binaries before tagging the extension.

## Cross-artifact dependency gotchas (the things users forget)

Read these before tagging. Each has burned at least one release.

1. **`bootstrap.sh` fetches the latest Rust release tarball.** If you change `install.sh` (e.g. adding a `--skip-build` flag), users only get the new behavior **after** you push a new `v*` tag — `bootstrap.sh` lives on `main` but the install logic comes from the tarball. Touched `install.sh`? You probably need a Rust release.

2. **`release.yml` packages `install.sh` into the tarball.** Same logic — `install.sh` updates need a Rust release to actually reach users.

3. **`layouts/*.md` is a single source of truth across both halves.** TUI uses `include_str!` (compile-time embed); the VSCode extension copies them to `dist/layouts/` at compile time. Touching them means **both** halves need re-releasing.

4. **VSCode extension's `package.json::version` MUST match the tag.** The workflow rejects mismatched tags. Bump `package.json` first, commit, *then* tag.

5. **Tag pattern collision (now fixed but check anyway).** `release.yml` matches `v[0-9]*.*.*` (anchored on a digit) so `vscode-extension-v0.2.0` no longer double-fires. If you see a `release.yml` run for a `vscode-extension-*` tag, it's a regression — cancel the run and re-check the matcher.

6. **Tarball must contain all five binaries.** `install.sh` hard-fails when `cc-connect-mcp` is missing; warns when `cc-connect-tui` is missing. Both must be in the `cp` loop in `release.yml::Package` step.

## Step-by-step: VSCode extension release

```bash
# 1. Check working tree + branch
git status                                 # Should be clean
git branch --show-current                  # Should be main
git fetch origin && git status -sb         # Confirm in sync with origin

# 2. Determine next version
node -p "require('./vscode-extension/package.json').version"
# Default: bump minor for substantial features, patch for bug-fix-only.

# 3. Bump package.json version (hand-edit or jq)
$EDITOR vscode-extension/package.json     # version: "X.Y.Z" → "X.Y'.Z'"

# 4. Commit + push the bump
git add vscode-extension/package.json
git commit -m "chore(vscode-extension): bump to X.Y.Z"
git push origin main

# 5. Tag and push
git tag -a vscode-extension-vX.Y.Z -m "vscode-extension-vX.Y.Z — <one-line summary>

<2-5 line bullet list of major changes since last extension release>"
git push origin vscode-extension-vX.Y.Z

# 6. Watch CI (≈25 seconds — pure TS bundle + vsce package)
gh run watch --exit-status            # or gh run list, looking for vscode-extension-release.yml

# 7. Verify the release page has the .vsix attached
gh release view vscode-extension-vX.Y.Z
```

## Step-by-step: Rust binary release

```bash
# 1. Same hygiene as above — clean tree, on main, in sync.
git status; git branch --show-current; git fetch origin

# 2. Determine next version
git tag --list 'v[0-9]*' --sort=-v:refname | head -1
# Default for pre-1.0: bump alpha number (v0.5.0-alpha → v0.5.1-alpha) for
# small fixes, minor (v0.5.0-alpha → v0.6.0-alpha) for features.

# 3. (Optional) bump Cargo.toml workspace.package.version for hygiene.
#    The CI workflow doesn't read it — version comes from the tag —
#    but keeping it in sync helps `cargo --version`-style introspection.

# 4. Tag and push
git tag -a vX.Y.Z[-alpha] -m "vX.Y.Z[-alpha] — <one-line summary>

<3-6 line bullets covering protocol/CLI/install changes>"
git push origin vX.Y.Z[-alpha]

# 5. Watch CI (≈10-15 min — three-platform Rust workspace build)
gh run watch --exit-status            # release.yml takes the longest

# 6. Verify three tarballs + their .sha256 siblings landed
gh release view vX.Y.Z[-alpha]
```

## Both halves at once

Always do **Rust first**, then VSCode extension. The extension's behavior may depend on the binary it shells out to.

```bash
# 1. Cut Rust release, wait for CI green
git tag -a v0.6.0-alpha -m "..."
git push origin v0.6.0-alpha
gh run watch --exit-status                # ~10-15 min

# 2. (Optional but recommended) Smoke-test the new tarball:
curl -fsSL https://raw.githubusercontent.com/Minara-AI/cc-connect/main/scripts/bootstrap.sh \
  | CC_CONNECT_VERSION=v0.6.0-alpha bash
~/.local/bin/cc-connect doctor

# 3. Cut extension release
$EDITOR vscode-extension/package.json     # bump
git commit -am "chore(vscode-extension): bump to 0.3.0"
git push origin main
git tag -a vscode-extension-v0.3.0 -m "..."
git push origin vscode-extension-v0.3.0
gh run watch --exit-status                # ~25s
```

## Pre-flight checklist

Before tagging, verify:

- [ ] **Working tree is clean** (`git status` shows nothing).
- [ ] **On `main`** and in sync with `origin/main` (`git status -sb` doesn't say "ahead/behind").
- [ ] **Local build passes** — `bun run compile` from `vscode-extension/` (extension) or `cargo build --workspace --release` (Rust). CI does this too, but a 30-second local check is cheaper than a 15-minute CI fail.
- [ ] **`gh` CLI is authenticated** — `gh auth status`. Without it, you can tag but can't watch CI or verify the release.
- [ ] **lifecycle.rs is in sync** *(Rust release-shaped PRs only)* — see the [Release checklist in CLAUDE.md](../../../CLAUDE.md). New binary, new ~/.claude/* key, new persistent file outside ~/.cc-connect/? `cc-connect uninstall --purge` must be able to reverse it.
- [ ] **`vscode-extension/package.json::version` matches the tag** *(extension releases only)*. The workflow refuses mismatched tags; better to catch it pre-push.

## Release notes (the body of the GitHub Release)

`softprops/action-gh-release` auto-generates notes from commits since the last tag of the **same** namespace. The annotated tag message you supply via `-m` becomes the lead — keep it tight:

- One headline sentence summarising the release theme.
- 3-6 bullets, ranked by user impact: features → bug fixes → internal refactors.
- Mention any follow-up that's still pending (e.g. "Marketplace publish deferred to vscode-extension-v0.3.x").

## Watching CI

```bash
gh run watch --exit-status                # blocks on the most recent run
gh run list --limit 5                     # see the queue
gh run view <run-id> --log-failed         # if it red, dump failed logs
```

If CI fails:

- **`vscode-extension-release.yml`** validation: tag and `package.json::version` disagree. Bump the file, amend the bump commit, force-push (only main has the bump; the tag points at the *same* commit you'd amend, so `git tag -f vscode-extension-vX.Y.Z` after the amend then `git push origin vscode-extension-vX.Y.Z --force`).
- **`release.yml`** Rust build failure: usually a flake or a real compile error on a less-tested platform. `gh run rerun <id>` for flakes; otherwise fix locally and re-tag with the next patch version (don't force-move release tags — users may have already downloaded the artifacts).

## Stage B (deferred)

Marketplace auto-publish via `vsce publish` is intentionally not wired:

- Needs an Azure DevOps PAT stored as `secrets.VSCE_PAT`.
- Needs the `minara` publisher account verified.
- Should only run on stable tags (no `-rc.1` / `-alpha`).

When ready, add a final step to `vscode-extension-release.yml`:

```yaml
- name: Publish to Marketplace (stable only)
  if: ${{ !contains(github.event.inputs.tag || github.ref_name, '-') }}
  working-directory: vscode-extension
  env:
    VSCE_PAT: ${{ secrets.VSCE_PAT }}
  run: bunx @vscode/vsce publish --packagePath cc-connect-vscode-${{ steps.meta.outputs.version }}.vsix
```

## Reference

- Tag namespace rationale + worked examples: [`README.md` "Release tag namespaces"](../../../README.md#release-tag-namespaces)
- Cleanup contract for releases that change install surface: [`CLAUDE.md` "Release checklist"](../../../CLAUDE.md)
- Extension-side gotchas (launcher prompts, bootstrap-once-per-runner): [`vscode-extension/CLAUDE.md`](../../../vscode-extension/CLAUDE.md)
