#!/usr/bin/env bash
# cc-connect tmux launcher. Spawns a session with claude L + cc-chat-ui R,
# both inheriting CC_CONNECT_ROOM so claude's hook fires and chat-ui
# finds the right chat.sock.
#
# `__CLAUDE_WRAPPER__` is substituted at launch by room.rs to the path
# of `claude-wrap.sh`, which prepends `--append-system-prompt` from the
# auto-reply prompt file if room.rs wrote one (the cc_wait_for_mention
# auto-reply loop). Falls through to plain claude if not.
#
# Embedded into the cc-connect binary via include_str! and written to a
# tmpfile at launch time. Reads CC_CONNECT_ROOM from env.
set -euo pipefail

if [ -z "${CC_CONNECT_ROOM:-}" ]; then
  echo "cc-connect.tmux.sh: CC_CONNECT_ROOM env var not set" >&2
  exit 2
fi

SESSION="${CC_CONNECT_TMUX_SESSION:-cc-connect-${CC_CONNECT_ROOM:0:12}}"
CLAUDE_LAUNCH="${CC_CONNECT_CLAUDE_LAUNCHER:-__CLAUDE_WRAPPER__}"
CHAT_UI_BIN="${CC_CONNECT_CHAT_UI_BIN:-cc-chat-ui}"

# If the session already exists, just attach. Lets a user re-attach a
# room they detached from earlier.
if tmux has-session -t "$SESSION" 2>/dev/null; then
  exec tmux attach-session -t "$SESSION"
fi

# Otherwise build it. -d (detached) so we can compose before attaching.
# -e exports CC_CONNECT_ROOM + CC_CONNECT_AUTO_REPLY_FILE into each
# pane's environment so the hook + chat-ui + the claude wrapper all
# pick up the right values without us having to send-keys.
tmux new-session -d -s "$SESSION" -x 220 -y 50 \
  -e "CC_CONNECT_ROOM=$CC_CONNECT_ROOM" \
  -e "CC_CONNECT_AUTO_REPLY_FILE=${CC_CONNECT_AUTO_REPLY_FILE:-}" \
  "$CLAUDE_LAUNCH"
tmux split-window -h -t "$SESSION" -p 40 \
  -e "CC_CONNECT_ROOM=$CC_CONNECT_ROOM" \
  "$CHAT_UI_BIN"
tmux select-pane -t "$SESSION".0

exec tmux attach-session -t "$SESSION"
