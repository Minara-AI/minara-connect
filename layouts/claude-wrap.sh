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
if [ "$#" -gt 0 ]; then
  case "$1" in
    [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f])
      export CC_CONNECT_ROOM="$1"
      shift
      ;;
  esac
fi

PROMPT_FILE="${CC_CONNECT_AUTO_REPLY_FILE:-${TMPDIR:-/tmp}/cc-connect-$(id -u)/auto-reply.md}"
CLAUDE="${CC_CONNECT_CLAUDE_BIN:-claude}"

if [ -z "${CC_CONNECT_NO_AUTO_REPLY:-}" ] && [ -f "$PROMPT_FILE" ]; then
  exec "$CLAUDE" --append-system-prompt "$(cat "$PROMPT_FILE")" "$@"
fi
exec "$CLAUDE" "$@"
