#!/bin/sh
# cc-connect claude wrapper.
#
# Spawned in place of `claude` by the room launcher (zellij KDL or tmux
# script). Two responsibilities:
#
#   1. If the first arg is a 64-char hex topic, export it as
#      CC_CONNECT_ROOM. This is how zellij's `action new-tab` path
#      passes the topic into a freshly spawned pane — env vars from
#      the parent invocation don't propagate through zellij's daemon.
#
#   2. If $CC_CONNECT_AUTO_REPLY_FILE points at an existing file —
#      room.rs writes that file unless CC_CONNECT_NO_AUTO_REPLY=1 —
#      claude starts with that file's content appended to its system
#      prompt, which arms the auto-reply listener loop. Otherwise
#      falls through to plain claude.
#
# Embedded into the cc-connect binary via include_str! and written to
# /tmp/cc-connect-$UID/claude-wrap.sh at launch time.

# If first arg looks like a topic hex (64 chars, lowercase hex), consume
# it as CC_CONNECT_ROOM. Anything else falls through to claude verbatim.
#
# Length + content split (rather than `[0-9a-f]` × 64 in a single case
# pattern) so we can't quietly miscount the brackets. POSIX-portable.
if [ "$#" -gt 0 ] && [ "${#1}" -eq 64 ]; then
  case "$1" in
    *[!0-9a-f]*) ;;  # contains a non-hex char — not a topic
    *)
      export CC_CONNECT_ROOM="$1"
      shift
      ;;
  esac
fi

PROMPT_FILE="${CC_CONNECT_AUTO_REPLY_FILE:-${TMPDIR:-/tmp}/cc-connect-$(id -u)/auto-reply.md}"
BOOTSTRAP_FILE="${CC_CONNECT_BOOTSTRAP_FILE:-${TMPDIR:-/tmp}/cc-connect-$(id -u)/bootstrap.md}"
CLAUDE="${CC_CONNECT_CLAUDE_BIN:-claude}"

# Default permission-mode flag. cc-connect launches claude inside a
# trusted-substrate room where each prompt fires the UserPromptSubmit
# hook + MCP tools — interactive permission prompts break that flow.
# Opt out by exporting CC_CONNECT_NO_PERMISSION_BYPASS=1 (the bypass
# is then dropped and claude reverts to its built-in default).
if [ -z "${CC_CONNECT_NO_PERMISSION_BYPASS:-}" ]; then
  set -- --permission-mode bypassPermissions "$@"
fi

# When both files exist (room.rs writes them at launch unless
# CC_CONNECT_NO_AUTO_REPLY=1), claude boots with:
#   - the auto-reply directive appended to its system prompt
#   - the bootstrap message as its first user prompt (which kicks
#     it into "say hello + enter listener loop" without the user
#     having to type anything first)
# Bootstrap content lives in layouts/bootstrap-prompt.md so the TUI
# path (cc-connect-tui) can include the same string via include_str!.
if [ -z "${CC_CONNECT_NO_AUTO_REPLY:-}" ] && [ -f "$PROMPT_FILE" ] && [ -f "$BOOTSTRAP_FILE" ]; then
  exec "$CLAUDE" --append-system-prompt "$(cat "$PROMPT_FILE")" "$@" "$(cat "$BOOTSTRAP_FILE")"
fi
exec "$CLAUDE" "$@"
