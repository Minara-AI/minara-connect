#!/usr/bin/env bash
# Install cc-connect's git hooks.
#
# We use a tracked .githooks/ directory rather than husky/lefthook to keep the
# install zero-dependency for Rust-only contributors. The hooks themselves
# dispatch by staged-file path so Bun is only required when chat-ui/ changes.
#
# Idempotent. Safe to run repeatedly.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$repo_root" ]; then
  echo "error: not inside a git repo" >&2
  exit 1
fi

cd "$repo_root"

if [ ! -d .githooks ]; then
  echo "error: .githooks/ not found in $repo_root" >&2
  exit 1
fi

# Make the hook scripts executable. Required on fresh checkouts.
chmod +x .githooks/* 2>/dev/null || true

current="$(git config --local --get core.hooksPath || true)"
if [ "$current" = ".githooks" ]; then
  echo "[git-hooks] already configured (core.hooksPath=.githooks)"
else
  git config --local core.hooksPath .githooks
  echo "[git-hooks] core.hooksPath set to .githooks"
fi

echo "[git-hooks] installed:"
for h in .githooks/*; do
  [ -f "$h" ] || continue
  echo "  - $(basename "$h")"
done

cat <<'EOF'

[git-hooks] Bypass any hook with `git commit --no-verify`.
[git-hooks] CI runs the same checks, so bypass only when you must.
EOF
