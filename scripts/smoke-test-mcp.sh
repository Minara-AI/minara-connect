#!/usr/bin/env bash
# End-to-end smoke test for the v0.3 MCP layer:
#   - cc_send via cc-connect-mcp lands in another peer's log (gossip path)
#   - cc_drop via cc-connect-mcp updates INDEX.md on the receiver
#   - cc_save_summary writes summary.md
#   - cc-connect-hook composes [cc-connect summary] + [cc-connect files] +
#     verbatim chat lines into one prompt-bound payload
#
# Run with:  ./scripts/smoke-test-mcp.sh
# Requires:  cargo build --workspace --release  has run.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REL="$REPO_ROOT/target/release"

for bin in cc-connect cc-connect-hook cc-connect-mcp; do
  if [[ ! -x "$REL/$bin" ]]; then
    echo "missing $REL/$bin — run: cargo build --workspace --release" >&2
    exit 2
  fi
done

WORK=$(mktemp -d)
HOME_HOST="$WORK/host"
HOME_A="$WORK/peer-a"
HOME_B="$WORK/peer-b"
TMP_HOST="$WORK/tmp-host"
TMP_A="$WORK/tmp-a"
TMP_B="$WORK/tmp-b"
mkdir -p "$HOME_HOST" "$HOME_A" "$HOME_B" "$TMP_HOST" "$TMP_A" "$TMP_B"

PIDS=()
cleanup() {
  rc=$?
  for p in "${PIDS[@]:-}"; do
    { kill "$p" 2>/dev/null && wait "$p" 2>/dev/null; } &
  done
  wait 2>/dev/null || true
  if [[ $rc -ne 0 ]]; then
    echo
    echo "===== FAIL — leaving $WORK for inspection ====="
    for f in "$WORK"/host.out "$WORK"/a.out "$WORK"/b.out; do
      [[ -f "$f" ]] && { echo "--- $f ---"; tail -40 "$f"; }
    done
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

# ---- 1. boot host -----------------------------------------------------------
HOME="$HOME_HOST" TMPDIR="$TMP_HOST" "$REL/cc-connect" host --no-relay \
  > "$WORK/host.out" 2>&1 &
PIDS+=($!)

TICKET=""
for _ in $(seq 1 60); do
  TICKET=$(grep -m1 -E '^[[:space:]]*cc1-[[:alnum:]]+' "$WORK/host.out" 2>/dev/null | tr -d ' ' || true)
  [[ -n "$TICKET" ]] && break
  sleep 0.5
done
[[ -n "$TICKET" ]] || { echo "FAIL: host never printed a ticket"; exit 1; }
echo "[mcp-smoke] host up"

# ---- 2. peers ---------------------------------------------------------------
mkfifo "$WORK/a.in" "$WORK/b.in"
HOME="$HOME_A" TMPDIR="$TMP_A" "$REL/cc-connect" chat "$TICKET" --no-relay \
  < "$WORK/a.in" > "$WORK/a.out" 2>&1 &
PIDS+=($!)
HOME="$HOME_B" TMPDIR="$TMP_B" "$REL/cc-connect" chat "$TICKET" --no-relay \
  < "$WORK/b.in" > "$WORK/b.out" 2>&1 &
PIDS+=($!)
exec 7> "$WORK/a.in"
exec 8> "$WORK/b.in"

for _ in $(seq 1 60); do
  if grep -q 'Joined room:' "$WORK/a.out" 2>/dev/null \
     && grep -q 'Joined room:' "$WORK/b.out" 2>/dev/null; then
    break
  fi
  sleep 0.5
done
grep -q 'Joined room:' "$WORK/a.out" || { echo "FAIL: peer A never joined"; exit 1; }
grep -q 'Joined room:' "$WORK/b.out" || { echo "FAIL: peer B never joined"; exit 1; }

# Extract the topic_id_hex from peer A's banner line: "Joined room: <12-char-prefix> ..."
TOPIC_PREFIX=$(grep -m1 'Joined room:' "$WORK/a.out" | awk '{print $3}')
# We need the FULL 64-char topic to set CC_CONNECT_ROOM. The chat session
# writes <HOME>/.cc-connect/rooms/<full_topic>/log.jsonl — look there.
FULL_TOPIC=$(ls "$HOME_A/.cc-connect/rooms" | head -1)
[[ -n "$FULL_TOPIC" ]] || { echo "FAIL: no rooms dir on peer A"; exit 1; }
echo "[mcp-smoke] both peers joined; topic = ${TOPIC_PREFIX}…"

sleep 2 # let gossip mesh

# ---- 3. cc_send via MCP from A → B -----------------------------------------
SENT_BODY="hello-from-mcp-$(date +%s)"
mcp_call() {
  local home="$1" topic="$2" payload="$3"
  echo "$payload" | HOME="$home" TMPDIR="$TMP_A" CC_CONNECT_ROOM="$topic" \
    "$REL/cc-connect-mcp"
}

SEND_RESP=$(mcp_call "$HOME_A" "$FULL_TOPIC" \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"cc_send","arguments":{"body":"'"$SENT_BODY"'"}}}')

echo "$SEND_RESP" | python3 -c "
import json, sys
d = json.loads(sys.stdin.readline())
assert d.get('result',{}).get('content',[{}])[0].get('type') == 'text', d
text = d['result']['content'][0]['text']
assert 'sent' in text, text
" || { echo "FAIL: cc_send response unexpected:"; echo "$SEND_RESP"; exit 1; }
echo "[mcp-smoke] cc_send OK"

# Wait for B to receive + log it.
for _ in $(seq 1 30); do
  if grep -q "$SENT_BODY" "$HOME_B/.cc-connect/rooms/$FULL_TOPIC/log.jsonl" 2>/dev/null; then
    break
  fi
  sleep 1
done
grep -q "$SENT_BODY" "$HOME_B/.cc-connect/rooms/$FULL_TOPIC/log.jsonl" \
  || { echo "FAIL: peer B never received the MCP-sent message"; exit 1; }
echo "[mcp-smoke] PASS: B's log contains the MCP-sent body"

# ---- 4. cc_drop via MCP ----------------------------------------------------
DROP_FILE="$WORK/mcp-drop.bin"
head -c 1024 /dev/urandom > "$DROP_FILE"
EXPECT_SHA=$(if command -v sha256sum >/dev/null 2>&1; then sha256sum "$DROP_FILE" | awk '{print $1}'; else shasum -a 256 "$DROP_FILE" | awk '{print $1}'; fi)
DROP_RESP=$(mcp_call "$HOME_A" "$FULL_TOPIC" \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"cc_drop","arguments":{"path":"'"$DROP_FILE"'"}}}')
echo "$DROP_RESP" | grep -q '"isError":true' \
  && { echo "FAIL: cc_drop returned isError:"; echo "$DROP_RESP"; exit 1; }
echo "[mcp-smoke] cc_drop OK"

B_DROP_GLOB="$HOME_B/.cc-connect/rooms/$FULL_TOPIC/files/*-mcp-drop.bin"
FOUND=""
for _ in $(seq 1 30); do
  for f in $B_DROP_GLOB; do
    [[ -f "$f" ]] && { FOUND="$f"; break; }
  done
  [[ -n "$FOUND" ]] && break
  sleep 1
done
[[ -n "$FOUND" ]] || { echo "FAIL: B never received the dropped file"; exit 1; }
GOT_SHA=$(if command -v sha256sum >/dev/null 2>&1; then sha256sum "$FOUND" | awk '{print $1}'; else shasum -a 256 "$FOUND" | awk '{print $1}'; fi)
[[ "$GOT_SHA" == "$EXPECT_SHA" ]] || { echo "FAIL: hash mismatch on B"; exit 1; }
echo "[mcp-smoke] PASS: B's files dir has the dropped file with matching sha"

# Verify B's INDEX.md was updated.
INDEX="$HOME_B/.cc-connect/rooms/$FULL_TOPIC/files/INDEX.md"
[[ -f "$INDEX" ]] || { echo "FAIL: B's INDEX.md missing"; exit 1; }
grep -q 'mcp-drop.bin' "$INDEX" \
  || { echo "FAIL: INDEX.md missing the new entry; got:"; cat "$INDEX"; exit 1; }
echo "[mcp-smoke] PASS: B's INDEX.md updated"

# ---- 5. cc_save_summary via MCP from B ------------------------------------
SUMMARY_TEXT="Discussed: MCP wiring works. Files: mcp-drop.bin shared. Action: smoke test passes."
SUM_RESP=$(echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cc_save_summary","arguments":{"text":"'"$SUMMARY_TEXT"'"}}}' \
  | HOME="$HOME_B" TMPDIR="$TMP_B" CC_CONNECT_ROOM="$FULL_TOPIC" "$REL/cc-connect-mcp")
echo "$SUM_RESP" | grep -q '"isError":true' \
  && { echo "FAIL: cc_save_summary returned isError:"; echo "$SUM_RESP"; exit 1; }
SUMMARY_FILE="$HOME_B/.cc-connect/rooms/$FULL_TOPIC/summary.md"
[[ -f "$SUMMARY_FILE" ]] || { echo "FAIL: summary.md not written"; exit 1; }
grep -q 'MCP wiring works' "$SUMMARY_FILE" \
  || { echo "FAIL: summary.md content missing"; exit 1; }
echo "[mcp-smoke] PASS: cc_save_summary wrote B's summary.md"

# ---- 6. Hook injection assembles all three sections -----------------------
# Run cc-connect-hook from B's HOME with a synthetic prompt — it should
# emit [cc-connect summary], [cc-connect files], and the verbatim
# chat lines including our cc_send body.
HOOK_OUT=$(echo '{"session_id":"smoke-mcp"}' \
  | HOME="$HOME_B" TMPDIR="$TMP_B" "$REL/cc-connect-hook")

echo "$HOOK_OUT" | grep -q '\[cc-connect summary\]' \
  || { echo "FAIL: hook output missing [cc-connect summary]"; echo "--- hook ---"; echo "$HOOK_OUT"; exit 1; }
echo "$HOOK_OUT" | grep -q 'MCP wiring works' \
  || { echo "FAIL: hook output missing summary body"; exit 1; }
echo "$HOOK_OUT" | grep -q '\[cc-connect files\]' \
  || { echo "FAIL: hook output missing [cc-connect files]"; exit 1; }
echo "$HOOK_OUT" | grep -q 'mcp-drop.bin' \
  || { echo "FAIL: hook output missing files entry"; exit 1; }
echo "$HOOK_OUT" | grep -q "$SENT_BODY" \
  || { echo "FAIL: hook output missing the MCP-sent chat line"; exit 1; }
echo "[mcp-smoke] PASS: hook output composes summary + files + verbatim"

echo
echo "===== ALL OK ====="
