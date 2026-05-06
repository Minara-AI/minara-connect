#!/usr/bin/env bash
# bootstrap.sh — one-liner install for cc-connect.
#
# Default path (no Rust required):
#   curl -fsSL https://raw.githubusercontent.com/Minara-AI/cc-connect/main/scripts/bootstrap.sh | bash
#
# Downloads the latest release tarball matching your platform,
# verifies its sha256, extracts to ~/.cc-connect-cache/<tag>/, and
# runs the matching install.sh in --skip-build mode (which wires the
# UserPromptSubmit hook + cc-connect-mcp server, then symlinks
# binaries into ~/.local/bin/ and runs `cc-connect doctor`).
#
# Build-from-source path (if you want to hack on cc-connect or the
# latest release doesn't cover your platform):
#   curl -fsSL <…/bootstrap.sh> | CC_CONNECT_FROM_SOURCE=1 bash
#
# Pin a specific version (e.g. for reproducible CI bootstraps):
#   curl -fsSL <…/bootstrap.sh> | CC_CONNECT_VERSION=v0.1.0 bash
#
# Override install location for the source build (default
# ~/cc-connect):
#   curl -fsSL <…/bootstrap.sh> | CC_CONNECT_DIR=/opt/cc-connect bash

set -euo pipefail

REPO=https://github.com/Minara-AI/cc-connect.git
RAW_BASE=https://raw.githubusercontent.com/Minara-AI/cc-connect
RELEASES_BASE=https://github.com/Minara-AI/cc-connect/releases
API_LATEST=https://api.github.com/repos/Minara-AI/cc-connect/releases/latest

# ─── helpers ──────────────────────────────────────────────────────────────────

die()  { printf "\033[1;31m✗\033[0m %s\n" "$*" >&2; exit 1; }
say()  { printf "\033[1;36m▶\033[0m %s\n" "$*"; }
ok()   { printf "\033[1;32m✓\033[0m %s\n" "$*"; }
warn() { printf "\033[1;33m!\033[0m %s\n" "$*" >&2; }

need() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required ($2)"
}

# ─── platform detection ───────────────────────────────────────────────────────

detect_target() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "$os/$arch" in
    Darwin/arm64)        echo aarch64-apple-darwin ;;
    Linux/x86_64)        echo x86_64-unknown-linux-gnu ;;
    Darwin/x86_64)
      # Apple Intel was dropped from release.yml's build matrix
      # (GitHub's public macos-13 runner pool sustains multi-hour
      # queues for tagged releases). Intel Macs build from source.
      die "no pre-built tarball for Apple Intel (x86_64-apple-darwin). Set CC_CONNECT_FROM_SOURCE=1 to build from source — needs Rust ≥ 1.89." ;;
    Linux/aarch64)
      # Pre-built linux-aarch64 isn't currently in release.yml's
      # build matrix. Fall through to source build with a clear
      # message.
      die "no pre-built tarball for Linux aarch64 yet — set CC_CONNECT_FROM_SOURCE=1 to build from source." ;;
    *)
      die "unsupported platform: $os/$arch — set CC_CONNECT_FROM_SOURCE=1 to attempt a source build." ;;
  esac
}

# ─── source build path (CC_CONNECT_FROM_SOURCE=1) ─────────────────────────────

source_install() {
  need git "to clone the repo"
  local dest="${CC_CONNECT_DIR:-$HOME/cc-connect}"
  if [[ -d "$dest/.git" ]]; then
    say "cc-connect already cloned at $dest — pulling latest"
    git -C "$dest" pull --ff-only
  else
    say "cloning into $dest"
    git clone "$REPO" "$dest"
  fi
  cd "$dest"
  if [[ -r /dev/tty ]]; then
    ./install.sh </dev/tty
  else
    ./install.sh
  fi
  print_done_banner "$dest/target/release/cc-connect"
}

# ─── tarball path (default) ───────────────────────────────────────────────────

tarball_install() {
  need curl "to download the release tarball"
  need tar  "to extract the release tarball"
  need shasum "to verify the release sha256"

  local tag target version cache extracted tarball install_sh
  if [[ -n "${CC_CONNECT_VERSION:-}" ]]; then
    tag="$CC_CONNECT_VERSION"
    say "pinning to $tag (CC_CONNECT_VERSION)"
  else
    say "resolving latest release tag"
    tag=$(curl -fsSL "$API_LATEST" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
    [[ -n "$tag" ]] || die "could not parse latest release tag from $API_LATEST"
  fi

  target=$(detect_target)
  version="${tag#v}"
  cache="$HOME/.cc-connect-cache/$tag"
  tarball="cc-connect-${version}-${target}.tar.gz"
  extracted="$cache/cc-connect-${version}-${target}"

  mkdir -p "$cache"

  if [[ ! -d "$extracted" ]]; then
    say "downloading $tarball"
    curl -fSL --progress-bar \
      "$RELEASES_BASE/download/$tag/$tarball" -o "$cache/$tarball"
    curl -fsSL \
      "$RELEASES_BASE/download/$tag/$tarball.sha256" -o "$cache/$tarball.sha256"
    say "verifying sha256"
    ( cd "$cache" && shasum -a 256 -c "$tarball.sha256" >/dev/null ) \
      || die "checksum mismatch for $tarball — refusing to install"
    ok "checksum ok"
    say "extracting"
    tar -xzf "$cache/$tarball" -C "$cache"
  else
    ok "already extracted at $extracted"
  fi

  # The tarball ships only the release binaries + license/readme. The
  # registration logic lives in install.sh, which expects a
  # "$REPO_ROOT/target/release/<bin>" layout. Forge that layout, drop
  # the install.sh from the matching tag in beside it, then run it
  # with --skip-build so it skips the cargo / bun toolchain checks.
  mkdir -p "$extracted/target/release"
  for bin in cc-connect cc-connect-hook cc-connect-mcp cc-chat-ui; do
    if [[ -f "$extracted/$bin" && ! -e "$extracted/target/release/$bin" ]]; then
      ln -sf "../../$bin" "$extracted/target/release/$bin"
    fi
  done

  install_sh="$extracted/install.sh"
  if [[ ! -f "$install_sh" ]]; then
    say "fetching install.sh from tag $tag"
    curl -fsSL "$RAW_BASE/$tag/install.sh" -o "$install_sh"
    chmod +x "$install_sh"
  fi

  say "running install.sh --skip-build"
  cd "$extracted"
  if [[ -r /dev/tty ]]; then
    ./install.sh --skip-build </dev/tty
  else
    ./install.sh --skip-build --yes
  fi
  print_done_banner "$HOME/.local/bin/cc-connect"
}

# ─── done banner ──────────────────────────────────────────────────────────────

print_done_banner() {
  local cc_bin="$1"
  cat <<EOF

──────────────────────────────────────────────────────────────────
✓ cc-connect installed.

Recommended next steps:

  1. Restart Claude Code (Cmd-Q / fully quit, then reopen) so it
     picks up the new hook + MCP entries.

  2. Use it from VSCode (recommended):
       - Install the cc-connect VSCode extension (.vsix from the
         GitHub release page, or 'code --install-extension').
       - Click the cc-connect activity-bar icon → Start Room.

     Or use it from a terminal (TUI):
       $cc_bin room start
──────────────────────────────────────────────────────────────────
EOF
}

# ─── dispatch ─────────────────────────────────────────────────────────────────

if [[ "${CC_CONNECT_FROM_SOURCE:-}" == "1" ]]; then
  source_install
else
  tarball_install
fi
