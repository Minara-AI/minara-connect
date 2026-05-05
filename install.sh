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
  # Read from /dev/tty so `curl … | bash` (which pipes the script via stdin)
  # doesn't have its read consume bytes from the script body itself.
  local ans=""
  if [[ -r /dev/tty ]]; then
    read -r -p "$prompt $hint " ans </dev/tty || ans=""
  else
    read -r -p "$prompt $hint " ans || ans=""
  fi
  ans="${ans:-$default}"
  [[ "$ans" =~ ^[Yy]$ ]]
}

# ---------- 1. OS + toolchain check -------------------------------------------
case "$(uname -s)" in
  Darwin|Linux) ;;
  *) fail "unsupported OS: $(uname -s) — cc-connect targets macOS / Linux." ;;
esac
say "host: $(uname -s) $(uname -m)"

if [[ $SKIP_BUILD -eq 0 ]]; then
  # Source-build path: we need a working Rust toolchain.
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
      fail "Rust is required. Install rustc ≥ 1.89 then re-run install.sh."
    fi
  fi

  rust_ver="$(rustc --version 2>/dev/null | awk '{print $2}')"
  ok "rustc ${rust_ver}"

  # MSRV gate. Some iroh-stack deps use `edition = "2024"`, which requires
  # cargo ≥ 1.85. `cargo build` fails opaquely on older toolchains
  # ("feature `edition2024` is required"); compare the version up front and
  # offer to `rustup update` so users don't burn 5 minutes building before
  # the failure.
  required_rust="1.89.0"
  ver_lt() {
    # Returns 0 (true) when $1 < $2 by semver-ish ordering. Uses sort -V.
    local a="$1" b="$2"
    [[ "$a" == "$b" ]] && return 1
    [[ "$(printf '%s\n%s\n' "$a" "$b" | sort -V | head -1)" == "$a" ]]
  }
  if ver_lt "$rust_ver" "$required_rust"; then
    warn "rustc $rust_ver is older than the required $required_rust (some deps need edition 2024)."
    if command -v rustup >/dev/null 2>&1; then
      if confirm "Run \`rustup update stable\` now?" Y; then
        rustup update stable
        # Make sure the just-updated toolchain is the active default.
        rustup default stable >/dev/null 2>&1 || true
        rust_ver="$(rustc --version 2>/dev/null | awk '{print $2}')"
        ok "rustc ${rust_ver} (after update)"
        if ver_lt "$rust_ver" "$required_rust"; then
          fail "rustc still $rust_ver after update. Manually install a newer toolchain and re-run."
        fi
      else
        fail "Cannot continue with rustc $rust_ver. Run \`rustup update stable\` and re-run install.sh."
      fi
    else
      fail "rustc $rust_ver < $required_rust and rustup is not on PATH. Install Rust via https://rustup.rs and re-run."
    fi
  fi

  if ! command -v git >/dev/null 2>&1; then
    fail "git is required (and missing). Install via your package manager / Xcode CLI tools."
  fi
  ok "git $(git --version | awk '{print $3}')"
else
  # --skip-build path: we don't need rust / bun / git. Bootstrapped
  # tarballs land here. Just verify the prebuilt binaries are sitting
  # in $REPO_ROOT/target/release/.
  ok "skip-build mode — toolchain checks bypassed"
fi

# ---------- 1.5. multiplexer detection ----------------------------------------
# `cc-connect room start/join` prefers zellij, falls back to tmux, and
# falls back again to the embedded cc-connect-tui if neither is found.
# We don't *require* one — but the multi-pane chat-ui experience needs it.
have_zellij=0; have_tmux=0
command -v zellij >/dev/null 2>&1 && have_zellij=1
command -v tmux   >/dev/null 2>&1 && have_tmux=1
if [[ $have_zellij -eq 1 ]]; then
  ok "zellij $(zellij --version | awk '{print $2}')"
fi
if [[ $have_tmux -eq 1 ]]; then
  ok "tmux $(tmux -V | awk '{print $2}')"
fi
if [[ $have_zellij -eq 1 && $have_tmux -eq 1 ]]; then
  say "both zellij and tmux found — zellij will be preferred at launch"
fi
if [[ $have_zellij -eq 0 && $have_tmux -eq 0 ]]; then
  warn "neither zellij nor tmux found — \`cc-connect room\` will fall back to the"
  warn "embedded cc-connect-tui (single-window). For the recommended multi-pane"
  warn "chat-ui experience, install one of:"
  case "$(uname -s)" in
    Darwin)
      warn "  brew install zellij    # recommended"
      warn "  brew install tmux      # also fine"
      ;;
    Linux)
      warn "  cargo install --locked zellij    # zellij not always packaged"
      warn "  apt install tmux                 # debian/ubuntu"
      warn "  dnf install tmux                 # fedora"
      ;;
  esac
fi

# ---------- 1.6. bun + chat-ui build ------------------------------------------
# chat-ui (Bun + React + Ink) is the right pane of the multiplexer layout.
# Without bun installed, we skip the chat-ui build and the launcher's
# multiplexer paths fall back to cc-connect-tui.
need_chat_ui_build=0
if [[ $SKIP_BUILD -eq 0 ]]; then
  need_chat_ui_build=1
  if ! command -v bun >/dev/null 2>&1; then
    warn "bun not found — chat-ui (Bun + React + Ink) won't be built."
    if confirm "Install bun via the official installer (curl https://bun.sh/install | bash)?" Y; then
      curl -fsSL https://bun.sh/install | bash
      # bun's installer writes to ~/.bun/bin and updates shell rc; pull it
      # into THIS shell so the build below sees it.
      export BUN_INSTALL="${BUN_INSTALL:-$HOME/.bun}"
      export PATH="$BUN_INSTALL/bin:$PATH"
      if ! command -v bun >/dev/null 2>&1; then
        warn "bun installed but not on PATH yet. Open a new shell, then re-run:"
        warn "  ./install.sh --skip-build"
        need_chat_ui_build=0
      fi
    else
      warn "Skipping bun install. The multiplexer launcher will fall back to"
      warn "cc-connect-tui until you run \`./install.sh\` again with bun on PATH."
      need_chat_ui_build=0
    fi
  fi
  if command -v bun >/dev/null 2>&1; then
    ok "bun $(bun --version)"
  fi
fi

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
CHAT_UI_BIN="$REPO_ROOT/target/release/cc-chat-ui"
[[ -x "$CONNECT_BIN" ]] || fail "missing $CONNECT_BIN — re-run without --skip-build."
[[ -x "$HOOK_BIN" ]] || fail "missing $HOOK_BIN — re-run without --skip-build."
[[ -x "$MCP_BIN" ]] || fail "missing $MCP_BIN — re-run without --skip-build."

# ---------- 2.5. chat-ui build (Bun) ------------------------------------------
if [[ $need_chat_ui_build -eq 1 ]] && command -v bun >/dev/null 2>&1; then
  if [[ $SKIP_BUILD -eq 0 || ! -x "$CHAT_UI_BIN" ]]; then
    say "building chat-ui (bun install + bun build --compile)"
    ( cd "$REPO_ROOT/chat-ui" && bun install && bun run build )
    if [[ -x "$CHAT_UI_BIN" ]]; then
      ok "chat-ui built: $CHAT_UI_BIN"
    else
      warn "chat-ui build did not produce $CHAT_UI_BIN — multiplexer launches will fall back to cc-connect-tui."
    fi
  else
    ok "chat-ui already built: $CHAT_UI_BIN (--skip-build)"
  fi
else
  warn "skipping chat-ui build (bun unavailable or declined). Multiplexer launches will fall back to cc-connect-tui."
fi

# ---------- 3. wire the UserPromptSubmit hook ---------------------------------
CLAUDE_DIR="$HOME/.claude"
SETTINGS="$CLAUDE_DIR/settings.json"
# Canonical user-scope MCP config lives at ~/.claude.json (Claude Code's
# `claude mcp list` reads from this), NOT $CLAUDE_DIR/settings.json. We
# write to settings.json for the hook (which IS read from there) but to
# ~/.claude.json for MCP.
CLAUDE_JSON="$HOME/.claude.json"
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

install_mcp_via_claude_cli() {
  # Authoritative path: let Claude Code own the registration. This goes
  # to its canonical user-scope config (`~/.claude.json::mcpServers`)
  # via Claude Code's own logic, which several version of `claude` use
  # at startup. Returns 0 on success, 1 if `claude` isn't on PATH.
  local mcp_path="$1"
  command -v claude >/dev/null 2>&1 || return 1
  # Remove any prior entry (ignore failure — entry might not exist).
  claude mcp remove cc-connect --scope user >/dev/null 2>&1 || true
  claude mcp add cc-connect "$mcp_path" --scope user >/dev/null 2>&1
}

install_mcp_claudejson_jq() {
  # Fallback when `claude` CLI isn't available: write directly to
  # ~/.claude.json::mcpServers (the canonical user-scope MCP config
  # location, NOT ~/.claude/settings.json — that file's mcpServers is
  # only respected by some Claude Code versions). Idempotent.
  local mcp_path="$1" tmp
  tmp="$(mktemp)"
  if [[ -s "$CLAUDE_JSON" ]]; then
    jq --arg path "$mcp_path" '
      .mcpServers //= {} |
      .mcpServers["cc-connect"] = {command: $path, args: []}
    ' "$CLAUDE_JSON" > "$tmp"
  else
    jq -n --arg path "$mcp_path" '
      {mcpServers: {"cc-connect": {command: $path, args: []}}}
    ' > "$tmp"
  fi
  mv "$tmp" "$CLAUDE_JSON"
}

install_mcp_claudejson_python() {
  local mcp_path="$1"
  python3 - "$CLAUDE_JSON" "$mcp_path" <<'PY'
import json, sys, pathlib
cj_path, mcp_path = pathlib.Path(sys.argv[1]), sys.argv[2]
data = {}
if cj_path.exists() and cj_path.stat().st_size > 0:
    data = json.loads(cj_path.read_text())
servers = data.setdefault("mcpServers", {})
servers["cc-connect"] = {"command": mcp_path, "args": []}
cj_path.write_text(json.dumps(data, indent=2) + "\n")
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

if confirm "Install cc-connect MCP server (so Claude can chat back)?" Y; then
  if install_mcp_via_claude_cli "$MCP_BIN"; then
    ok "mcp server installed via 'claude mcp add' (canonical path)"
  else
    # Fallback: write to ~/.claude.json directly. Use jq if available,
    # python3 otherwise.
    if command -v jq >/dev/null 2>&1; then
      install_mcp_claudejson_jq "$MCP_BIN"
    elif command -v python3 >/dev/null 2>&1; then
      install_mcp_claudejson_python "$MCP_BIN"
    else
      fail "neither 'claude' CLI nor jq/python3 available — install one and re-run, or run 'claude mcp add cc-connect $MCP_BIN --scope user' manually."
    fi
    chmod 600 "$CLAUDE_JSON"
    ok "mcp server installed in $CLAUDE_JSON ('claude' CLI not on PATH)"
  fi
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
  for bin in cc-connect cc-connect-tui cc-connect-hook cc-connect-mcp cc-chat-ui; do
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

  cc-connect room launches a 2-pane terminal layout:
    LEFT  60% : claude (with CC_CONNECT_ROOM env so the hook fires)
    RIGHT 40% : cc-chat-ui (chat scrollback + input)
  Multiplexer preference: zellij > tmux > embedded cc-connect-tui (fallback).

  Multiplexer hotkeys you'll likely want:
    zellij : Ctrl-p arrow      switch pane
    zellij : Ctrl-q            quit (chat-ui owns Ctrl-Q inside its pane)
    tmux   : Ctrl-b arrow      switch pane (default prefix)
  Inside cc-chat-ui:
    Ctrl-Y copy ticket  ·  PgUp/PgDn scrollback  ·  Tab/Enter @-mention completion

  - Self-hosted relay: see README.md "Self-hosted relay (optional)".

Settings live at: $SETTINGS
Hook binary:      $HOOK_BIN
MCP binary:       $MCP_BIN
chat-ui binary:   $CHAT_UI_BIN
NEXT
