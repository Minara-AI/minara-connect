#!/bin/sh
# cc-connect claude wrapper.
#
# Spawned in place of `claude` by the room launcher (zellij KDL or tmux
# script). If $CC_CONNECT_AUTO_REPLY_FILE points at an existing file —
# room.rs writes that file unless CC_CONNECT_NO_AUTO_REPLY=1 — claude
# starts with that file's content appended to its system prompt, which
# arms the auto-reply listener loop. Otherwise falls through to plain
# claude.
#
# Embedded into the cc-connect binary via include_str! and written to
# /tmp/cc-connect-$UID/claude-wrap.sh at launch time.

PROMPT_FILE="${CC_CONNECT_AUTO_REPLY_FILE:-${TMPDIR:-/tmp}/cc-connect-$(id -u)/auto-reply.md}"
CLAUDE="${CC_CONNECT_CLAUDE_BIN:-claude}"

if [ -z "${CC_CONNECT_NO_AUTO_REPLY:-}" ] && [ -f "$PROMPT_FILE" ]; then
  exec "$CLAUDE" --append-system-prompt "$(cat "$PROMPT_FILE")" "$@"
fi
exec "$CLAUDE" "$@"
