# Contributing to cc-connect

Thanks for the interest. cc-connect is a small project with a strong opinion about its abstractions — the [Ubiquitous Language](./CONTEXT.md) is the load-bearing artifact. Reading `CONTEXT.md`, [`PROTOCOL.md`](./PROTOCOL.md), and the relevant ADRs in [`docs/adr/`](./docs/adr/) is the fastest way to make a contribution that lands.

## Quick start

```bash
git clone https://github.com/Minara-AI/cc-connect.git
cd cc-connect
./install.sh                       # builds, wires hook, registers MCP
scripts/install-git-hooks.sh       # contributor-only — installs pre-commit / commit-msg
```

That's it. The first build pulls the iroh stack and the vendored ed25519 patch (`vendored/`) and takes ~5–10 minutes.

## Toolchain

| Need              | Why                                                                  |
| ----------------- | -------------------------------------------------------------------- |
| Rust ≥ 1.75       | Workspace MSRV (`Cargo.toml` → `workspace.package.rust-version`)     |
| `bun` ≥ 1.1       | Build chat-ui (only needed if you touch `chat-ui/`)                  |
| `gh` CLI          | File issues / PRs                                                    |
| zellij **or** tmux| Optional. `cc-connect room start` falls back to the embedded TUI.    |

If you only touch Rust, you can skip Bun. The pre-commit hook only invokes Bun when `chat-ui/` is in the diff.

## Day-to-day commands

```bash
cargo build --workspace --release          # Rust workspace
(cd chat-ui && bun install && bun run build)   # chat-ui → target/release/cc-chat-ui
cargo test --workspace                     # unit + integration
scripts/smoke-test.sh                      # end-to-end gossip path
scripts/smoke-test-mcp.sh                  # MCP server tools
(cd chat-ui && bun test && bunx tsc --noEmit)
./target/release/cc-connect doctor         # verify your local install
```

## Style

- **Rust**: stable rustfmt, clippy with `-D warnings`. The pre-commit hook enforces both.
- **TypeScript** (chat-ui): Prettier (`chat-ui/.prettierrc`), `tsc --noEmit` for typecheck. No ESLint config; keep imports tidy and types explicit at component boundaries.
- **Naming**: stick to the [Ubiquitous Language](./CONTEXT.md). `Room`, `Ticket`, `Substrate`, `Peer`, `Host`, `Identity`, `Pubkey`, `Message`, `Hook`, `Cursor`, `Backfill`, `Session`, `Injection`, `Context`. New domain term? Update `CONTEXT.md` in the same change.

## Commits

`type(scope): subject`. Types: `feat | fix | chore | docs | refactor | test | perf | build | ci | revert`. Scopes are real components — `core`, `hook`, `mcp`, `tui`, `chat-ui`, `chat-daemon`, `install`, `room`, `security`, `protocol`, etc. Multi-area: `chore(install,setup): …`. Breaking changes: `feat(protocol)!: …`.

The `commit-msg` hook enforces this. `git log --oneline` shows the canon.

## Pull requests

Before opening:

1. Open an issue first if it's a non-trivial change. Architectural changes go through an [ADR](./docs/adr/) — see [`docs/agents/domain-docs.md`](./docs/agents/domain-docs.md) for when to write one.
2. `cargo fmt --all && cargo clippy --workspace --all-targets --tests -- -D warnings && cargo test --workspace`. CI runs the same matrix.
3. If you touch chat-ui: `(cd chat-ui && bun test && bunx tsc --noEmit && bunx prettier --check .)`.
4. If you touch the wire format: bump `v` and the ALPN per [`PROTOCOL.md`](./PROTOCOL.md) §0. Update `SECURITY.md` if the threat surface shifts.

PR title mirrors the eventual squashed commit subject. The body uses [the template](./.github/PULL_REQUEST_TEMPLATE.md):

- **Summary** — what + why, in 1–3 bullets.
- **Test plan** — concrete commands a reviewer can run. "I ran `cargo test`" is not enough; name what changed and how you verified it.

## Areas that need help

See [`TODOS.md`](./TODOS.md) for the v0.1 → v1.0 punch list. Issues labelled [`good-first-issue`](https://github.com/Minara-AI/cc-connect/labels/good-first-issue) are pre-scoped for outside contributors. Larger initiatives:

- **End-to-end Message signatures** (replace transport-only auth in v0.1; see [`SECURITY.md`](./SECURITY.md) "Known v0.1 weaknesses").
- **Backfill range reconciliation** — replace the bounded 50-message window with `iroh-docs` style sync if it proves insufficient (`docs/adr/0002-backfill-via-custom-rpc-not-iroh-docs.md`).
- **Cross-platform parity** — Linux is tested; Windows is not. Native packaging (deb/rpm/homebrew) wanted.

## Reporting bugs

Open a [GitHub issue](https://github.com/Minara-AI/cc-connect/issues/new/choose). Include:

- `cc-connect doctor` output.
- `rustc --version`, `bun --version` (if relevant), OS + version.
- The smallest reproduction you can get.

Maintainers triage with the labels in [`docs/agents/triage-labels.md`](./docs/agents/triage-labels.md).

## Reporting security issues

**Don't open a public issue for security vulnerabilities.** Use [GitHub's private vulnerability reporting](https://github.com/Minara-AI/cc-connect/security/advisories/new). [`SECURITY.md`](./SECURITY.md) covers the threat model and what's already known.

## Licensing

Contributions are dual-licensed under [MIT](./LICENSE-MIT) OR [Apache-2.0](./LICENSE-APACHE), matching the workspace. Submitting a PR is your acknowledgement of that — there is no separate CLA.

## Code of conduct

This project follows the [Contributor Covenant 2.1](./CODE_OF_CONDUCT.md). Be kind; assume good faith; no harassment.
