#!/usr/bin/env bash
# install.sh — interactive cc-connect setup.
#
# Walks the user through:
#   1. toolchain check (rustc / cargo / git)
#   2. workspace build (cargo build --workspace --release)
#   3. UserPromptSubmit hook wired into ~/.claude/settings.json
#   4. cc-connect doctor smoke check
#
# Re-runnable. Non-destructive: always backs up settings.json before edit.
# Pass --yes to accept every prompt; pass --skip-build to skip cargo build.

set -euo pipefail
trap 'rc=$?; printf "\033[1;31m✗\033[0m install.sh: failed at line %d (exit %d): %s\n" "$LINENO" "$rc" "${BASH_COMMAND}" >&2' ERR

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ASSUME_YES=0
SKIP_BUILD=0

for arg in "$@"; do
  case "$arg" in
    -y|--yes) ASSUME_YES=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    -h|--help)
      cat <<EOF
Usage: install.sh [--yes] [--skip-build]

  --yes          accept every confirmation prompt (non-interactive)
  --skip-build   skip 'cargo build --workspace --release' (use existing target/)
  -h, --help     show this help
EOF
      exit 0
      ;;
    *) echo "install.sh: unknown arg: $arg (try --help)" >&2; exit 2 ;;
  esac
done

# ---------- pretty output -----------------------------------------------------
say()    { printf "\033[1;36m▶\033[0m %s\n" "$*"; }
ok()     { printf "\033[1;32m✓\033[0m %s\n" "$*"; }
warn()   { printf "\033[1;33m!\033[0m %s\n" "$*"; }
fail()   { printf "\033[1;31m✗\033[0m %s\n" "$*" >&2; exit 1; }

confirm() {
  local prompt="$1" default="${2:-Y}"
  if [[ $ASSUME_YES -eq 1 ]]; then return 0; fi
  local hint="[Y/n]"; [[ "$default" == "n" ]] && hint="[y/N]"
  read -r -p "$prompt $hint " ans || ans=""
  ans="${ans:-$default}"
  [[ "$ans" =~ ^[Yy]$ ]]
}

# ---------- 1. OS + toolchain check -------------------------------------------
case "$(uname -s)" in
  Darwin|Linux) ;;
  *) fail "unsupported OS: $(uname -s) — cc-connect targets macOS / Linux." ;;
esac
say "host: $(uname -s) $(uname -m)"

need_rust=0
if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
  need_rust=1
fi
if [[ $need_rust -eq 1 ]]; then
  warn "rustc / cargo not found."
  if confirm "Install Rust via rustup?" Y; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # rustup writes its env to ~/.cargo/env; pull it into this shell.
    # shellcheck disable=SC1090,SC1091
    [[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
  else
    fail "Rust is required. Install rustc ≥ 1.85 then re-run install.sh."
  fi
fi

rust_ver="$(rustc --version 2>/dev/null | awk '{print $2}')"
ok "rustc ${rust_ver}"

if ! command -v git >/dev/null 2>&1; then
  fail "git is required (and missing). Install via your package manager / Xcode CLI tools."
fi
ok "git $(git --version | awk '{print $3}')"

# ---------- 2. workspace build ------------------------------------------------
if [[ $SKIP_BUILD -eq 0 ]]; then
  say "building workspace (release) — first build takes 5-10 min"
  ( cd "$REPO_ROOT" && cargo build --workspace --release )
  ok "build complete"
else
  warn "skipping cargo build (--skip-build)"
fi

CONNECT_BIN="$REPO_ROOT/target/release/cc-connect"
HOOK_BIN="$REPO_ROOT/target/release/cc-connect-hook"
MCP_BIN="$REPO_ROOT/target/release/cc-connect-mcp"
[[ -x "$CONNECT_BIN" ]] || fail "missing $CONNECT_BIN — re-run without --skip-build."
[[ -x "$HOOK_BIN" ]] || fail "missing $HOOK_BIN — re-run without --skip-build."
[[ -x "$MCP_BIN" ]] || fail "missing $MCP_BIN — re-run without --skip-build."

# ---------- 3. wire the UserPromptSubmit hook ---------------------------------
CLAUDE_DIR="$HOME/.claude"
SETTINGS="$CLAUDE_DIR/settings.json"
mkdir -p "$CLAUDE_DIR"

install_hook_jq() {
  # Pure-jq path. Idempotent: drops any entry already pointing at our hook
  # (in either the correct nested {matcher, hooks:[…]} shape OR the legacy
  # flat {type, command} shape that earlier install.sh runs wrote by
  # mistake), then appends the correct nested entry.
  local hook_path="$1" tmp
  tmp="$(mktemp)"
  if [[ -s "$SETTINGS" ]]; then
    jq --arg path "$hook_path" '
      .hooks //= {} |
      .hooks.UserPromptSubmit //= [] |
      .hooks.UserPromptSubmit |= map(select(
        ((.hooks // []) | map(.command) | index($path)) == null
        and (.command != $path)
      )) |
      .hooks.UserPromptSubmit += [
        {matcher: "", hooks: [{type: "command", command: $path}]}
      ]
    ' "$SETTINGS" > "$tmp"
  else
    jq -n --arg path "$hook_path" '
      {hooks: {UserPromptSubmit: [
        {matcher: "", hooks: [{type: "command", command: $path}]}
      ]}}
    ' > "$tmp"
  fi
  mv "$tmp" "$SETTINGS"
}

install_hook_python() {
  # Fallback when jq is unavailable. Python ships on every macOS / most Linux.
  local hook_path="$1"
  python3 - "$SETTINGS" "$hook_path" <<'PY'
import json, sys, pathlib
settings_path, hook_path = pathlib.Path(sys.argv[1]), sys.argv[2]
data = {}
if settings_path.exists() and settings_path.stat().st_size > 0:
    data = json.loads(settings_path.read_text())
hooks = data.setdefault("hooks", {})
ups = hooks.setdefault("UserPromptSubmit", [])

def points_at_us(entry):
    if not isinstance(entry, dict):
        return False
    # Legacy shape that earlier install.sh runs wrote by mistake.
    if entry.get("command") == hook_path:
        return True
    # Correct nested shape: {matcher, hooks: [{type, command}, …]}.
    for sub in entry.get("hooks") or []:
        if isinstance(sub, dict) and sub.get("command") == hook_path:
            return True
    return False

ups[:] = [h for h in ups if not points_at_us(h)]
ups.append({
    "matcher": "",
    "hooks": [{"type": "command", "command": hook_path}],
})
settings_path.write_text(json.dumps(data, indent=2) + "\n")
PY
}

install_mcp_jq() {
  # Register cc-connect-mcp in settings.json::mcpServers. Idempotent —
  # any existing "cc-connect" entry is overwritten. Other tools' entries
  # are left alone.
  local mcp_path="$1" tmp
  tmp="$(mktemp)"
  if [[ -s "$SETTINGS" ]]; then
    jq --arg path "$mcp_path" '
      .mcpServers //= {} |
      .mcpServers["cc-connect"] = {command: $path, args: []}
    ' "$SETTINGS" > "$tmp"
  else
    jq -n --arg path "$mcp_path" '
      {mcpServers: {"cc-connect": {command: $path, args: []}}}
    ' > "$tmp"
  fi
  mv "$tmp" "$SETTINGS"
}

install_mcp_python() {
  local mcp_path="$1"
  python3 - "$SETTINGS" "$mcp_path" <<'PY'
import json, sys, pathlib
settings_path, mcp_path = pathlib.Path(sys.argv[1]), sys.argv[2]
data = {}
if settings_path.exists() and settings_path.stat().st_size > 0:
    data = json.loads(settings_path.read_text())
servers = data.setdefault("mcpServers", {})
servers["cc-connect"] = {"command": mcp_path, "args": []}
settings_path.write_text(json.dumps(data, indent=2) + "\n")
PY
}

if [[ -f "$SETTINGS" ]]; then
  cp "$SETTINGS" "$SETTINGS.bak.$(date +%Y%m%d-%H%M%S)"
  ok "backed up existing settings.json"
fi

if confirm "Install UserPromptSubmit hook → $SETTINGS?" Y; then
  if command -v jq >/dev/null 2>&1; then
    install_hook_jq "$HOOK_BIN"
  elif command -v python3 >/dev/null 2>&1; then
    install_hook_python "$HOOK_BIN"
  else
    fail "neither jq nor python3 available — install one and re-run, or edit $SETTINGS manually."
  fi
  chmod 600 "$SETTINGS"
  ok "hook installed: $HOOK_BIN"
else
  warn "skipped settings.json edit. Hook NOT wired — \`cc-connect\` chats won't surface in Claude Code until you add the hook manually."
fi

if confirm "Install cc-connect MCP server → $SETTINGS::mcpServers?" Y; then
  if command -v jq >/dev/null 2>&1; then
    install_mcp_jq "$MCP_BIN"
  elif command -v python3 >/dev/null 2>&1; then
    install_mcp_python "$MCP_BIN"
  else
    fail "neither jq nor python3 available — install one and re-run, or edit $SETTINGS manually."
  fi
  chmod 600 "$SETTINGS"
  ok "mcp server installed: $MCP_BIN"
else
  warn "skipped MCP install. Claude in your room won't be able to call cc_send / cc_at / cc_drop / cc_save_summary etc."
fi

# ---------- 3.5. PATH symlinks (~/.local/bin) -------------------------------
# Without this, the user has to either type the absolute path or
# `export PATH=…` themselves. ~/.local/bin is on PATH by default on most
# distros + recent macOS; we print a tip if it isn't.
BIN_DIR="$HOME/.local/bin"
if confirm "Symlink binaries to $BIN_DIR so you can run them from any directory?" Y; then
  mkdir -p "$BIN_DIR"
  for bin in cc-connect cc-connect-tui cc-connect-hook cc-connect-mcp; do
    src="$REPO_ROOT/target/release/$bin"
    dest="$BIN_DIR/$bin"
    if [[ ! -x "$src" ]]; then
      warn "missing $src — skipping"
      continue
    fi
    ln -sf "$src" "$dest"
  done
  ok "binaries symlinked into $BIN_DIR"
  if ! echo ":$PATH:" | grep -q ":$BIN_DIR:"; then
    warn "$BIN_DIR is not on your PATH. Add to your shell rc (~/.zshrc / ~/.bashrc):"
    warn "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    warn "Then open a new shell, or:  hash -r"
  fi
else
  warn "skipped PATH symlinks. Use the absolute path: $REPO_ROOT/target/release/cc-connect …"
fi

# ---------- 4. doctor smoke check --------------------------------------------
say "running cc-connect doctor"
"$CONNECT_BIN" doctor || warn "doctor reported issues — review the output above."

# ---------- 5. next steps -----------------------------------------------------
cat <<NEXT

$(printf "\033[1;32m✓ install complete\033[0m")

Next steps:
  - Restart Claude Code so it picks up the new hook + MCP tools.
  - Recommended start:
        cc-connect room start          (if $BIN_DIR is on PATH)
        $REPO_ROOT/target/release/cc-connect room start    (otherwise)
  - Tab keys inside the TUI: 1-9 switch, Ctrl-N new, Ctrl-W close,
    F2 / Tab switch pane, Ctrl-Y copy ticket, Ctrl-Q quit.
  - Self-hosted relay: see README.md "Self-hosted relay (optional)".

Settings live at: $SETTINGS
Hook binary:      $HOOK_BIN
MCP binary:       $MCP_BIN
NEXT
