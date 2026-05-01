#!/usr/bin/env bash
# Configure the GitHub repository for cc-connect.
#
# Sets:
#   - description, homepage, topics
#   - merge / branch settings (squash-only, delete-branch-on-merge, no force push)
#   - issue labels (triage roles, kind:*, area:*, good-first-issue)
#   - branch protection on `main` (require PR, require CI checks passing)
#   - enable Discussions
#
# Idempotent: every call uses PATCH / PUT / "create-or-update", so running it
# repeatedly converges. Re-run after editing this file to apply changes.
#
# Prereq: `gh auth login` with admin access to `Minara-AI/cc-connect`.
# Required token scopes: `repo`, `admin:repo_hook` (for protection rules).

set -euo pipefail

REPO="${REPO:-Minara-AI/cc-connect}"
DEFAULT_BRANCH="${DEFAULT_BRANCH:-main}"

if ! command -v gh >/dev/null 2>&1; then
  echo "error: gh CLI not installed. https://cli.github.com" >&2
  exit 1
fi

if ! gh auth status -h github.com >/dev/null 2>&1; then
  echo "error: not authenticated to github.com — run \`gh auth login\`" >&2
  exit 1
fi

echo "==> Repo: $REPO  (default branch: $DEFAULT_BRANCH)"

# ---------------------------------------------------------------------------
# 1. Repo metadata + merge settings
# ---------------------------------------------------------------------------

echo "==> Updating repo metadata + merge settings"
gh api -X PATCH "repos/$REPO" \
  -f description="Peer-to-peer chat substrate that lets multiple Claude Code instances share the same context." \
  -f homepage="https://github.com/Minara-AI/cc-connect" \
  -F has_issues=true \
  -F has_projects=false \
  -F has_wiki=false \
  -F has_discussions=true \
  -F allow_squash_merge=true \
  -F allow_merge_commit=false \
  -F allow_rebase_merge=false \
  -F delete_branch_on_merge=true \
  -F allow_auto_merge=true \
  -F squash_merge_commit_title=PR_TITLE \
  -F squash_merge_commit_message=PR_BODY \
  >/dev/null

# Topics (idempotent; replaces the full set).
echo "==> Setting topics"
gh api -X PUT "repos/$REPO/topics" \
  -H "Accept: application/vnd.github+json" \
  -f 'names[]=claude-code' \
  -f 'names[]=p2p' \
  -f 'names[]=iroh' \
  -f 'names[]=collaboration' \
  -f 'names[]=ai-agents' \
  -f 'names[]=mcp' \
  -f 'names[]=rust' \
  -f 'names[]=tui' \
  >/dev/null

# ---------------------------------------------------------------------------
# 2. Labels
# ---------------------------------------------------------------------------

ensure_label() {
  local name="$1" color="$2" desc="$3"
  if gh label list -R "$REPO" --json name -q '.[].name' | grep -Fxq "$name"; then
    gh label edit "$name" -R "$REPO" --color "$color" --description "$desc" >/dev/null
  else
    gh label create "$name" -R "$REPO" --color "$color" --description "$desc" >/dev/null
  fi
  echo "    label: $name"
}

echo "==> Ensuring labels"

# Triage roles (see docs/agents/triage-labels.md).
ensure_label "needs-triage"    "fbca04" "Maintainer hasn't evaluated yet"
ensure_label "needs-info"      "d4c5f9" "Waiting on the reporter for clarification"
ensure_label "ready-for-agent" "0e8a16" "Fully specified — an AFK agent can pick this up cold"
ensure_label "ready-for-human" "1d76db" "Spec is clear but the work needs a human"
ensure_label "wontfix"         "ffffff" "Will not be actioned"

# Kind.
ensure_label "kind:bug"     "d73a4a" "Something doesn't work the way it should"
ensure_label "kind:feature" "a2eeef" "New capability"
ensure_label "kind:chore"   "fef2c0" "Tooling, deps, refactor without user-visible change"
ensure_label "kind:rfc"     "5319e7" "Architectural proposal — likely needs an ADR"

# Areas.
ensure_label "area:protocol" "c2e0c6" "Wire format / on-disk layout (PROTOCOL.md)"
ensure_label "area:hook"     "c2e0c6" "UserPromptSubmit hook"
ensure_label "area:mcp"      "c2e0c6" "MCP server"
ensure_label "area:tui"      "c2e0c6" "Terminal UI"
ensure_label "area:chat-ui"  "c2e0c6" "Bun + Ink chat panel"
ensure_label "area:install"  "c2e0c6" "install.sh / packaging"
ensure_label "area:security" "ee0701" "Threat model / hardening (SECURITY.md)"
ensure_label "area:docs"     "0075ca" "Documentation"

# Other.
ensure_label "good-first-issue" "7057ff" "Pre-scoped for outside contributors"
ensure_label "blocked"          "b60205" "Blocked on something external"

# ---------------------------------------------------------------------------
# 3. Branch protection
# ---------------------------------------------------------------------------
#
# Require:
#   - PRs only (no direct push to main)
#   - 1 review approval
#   - all CI checks passing (job names from .github/workflows/ci.yml)
#   - up-to-date branch before merge
#   - linear history (matches squash-only setting above)
#   - admin enforcement (no bypass)
#
# We don't gate on signed commits — open-source projects rarely do, and it
# locks out drive-by contributors. Revisit if threat model changes.

echo "==> Configuring branch protection on $DEFAULT_BRANCH"

protection_payload=$(cat <<'JSON'
{
  "required_status_checks": {
    "strict": true,
    "contexts": [
      "rustfmt",
      "clippy",
      "test (ubuntu-latest)",
      "test (macos-latest)",
      "MSRV (1.85)",
      "chat-ui (typecheck + test)"
    ]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": {
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": false,
    "required_approving_review_count": 1,
    "require_last_push_approval": false
  },
  "restrictions": null,
  "required_linear_history": true,
  "allow_force_pushes": false,
  "allow_deletions": false,
  "block_creations": false,
  "required_conversation_resolution": true,
  "lock_branch": false,
  "allow_fork_syncing": true
}
JSON
)

echo "$protection_payload" | gh api -X PUT \
  "repos/$REPO/branches/$DEFAULT_BRANCH/protection" \
  -H "Accept: application/vnd.github+json" \
  --input - >/dev/null

echo "==> Done."
echo
echo "Verify:"
echo "  gh repo view $REPO"
echo "  gh label list -R $REPO"
echo "  gh api repos/$REPO/branches/$DEFAULT_BRANCH/protection | jq ."
