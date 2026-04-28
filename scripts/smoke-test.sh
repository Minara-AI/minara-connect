#!/usr/bin/env bash
# Local end-to-end smoke test for v0.2 file_drop.
#
# Spins up host + peer A + peer B as three separate processes on this
# machine, each with its own HOME so they have distinct identities. Drops
# a 4 KiB random file from peer A and verifies peer B receives + exports
# it byte-for-byte.
#
# Run with:  ./scripts/smoke-test.sh
# Requires:  cargo build --workspace --release  has run.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REL="$REPO_ROOT/target/release"

if [[ ! -x "$REL/cc-connect" || ! -x "$REL/cc-connect-hook" ]]; then
  echo "missing binaries — run: cargo build --workspace --release" >&2
  exit 2
fi

# Pick sha256 tool.
if command -v sha256sum >/dev/null 2>&1; then
  SHA() { sha256sum "$1" | awk '{print $1}'; }
else
  SHA() { shasum -a 256 "$1" | awk '{print $1}'; }
fi

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
  # Silence "Terminated" job-control banners by killing in a subshell.
  for p in "${PIDS[@]:-}"; do
    { kill "$p" 2>/dev/null && wait "$p" 2>/dev/null; } &
  done
  wait 2>/dev/null || true
  if [[ $rc -ne 0 ]]; then
    echo
    echo "===== FAIL — leaving $WORK for inspection ====="
    echo "host.out:";  ls -la "$WORK"/*.out 2>/dev/null
    for f in "$WORK"/host.out "$WORK"/a.out "$WORK"/b.out; do
      [[ -f "$f" ]] && { echo "--- $f ---"; tail -40 "$f"; }
    done
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

# ---------- 1. Host ----------------------------------------------------------
HOME="$HOME_HOST" TMPDIR="$TMP_HOST" "$REL/cc-connect" host --no-relay \
  > "$WORK/host.out" 2>&1 &
PIDS+=($!)
HOST_PID=$!

# Wait for the ticket to land in stdout.
TICKET=""
for _ in $(seq 1 60); do
  TICKET=$(grep -m1 -E '^[[:space:]]*cc1-[[:alnum:]]+' "$WORK/host.out" 2>/dev/null | tr -d ' ' || true)
  [[ -n "$TICKET" ]] && break
  sleep 0.5
done
[[ -n "$TICKET" ]] || { echo "FAIL: host never printed a ticket"; exit 1; }
echo "[smoke] host up, ticket = ${TICKET:0:32}…"

# ---------- 2. Peers (with FIFO stdin so we can drive /drop) -----------------
mkfifo "$WORK/a.in" "$WORK/b.in"

HOME="$HOME_A" TMPDIR="$TMP_A" "$REL/cc-connect" chat "$TICKET" --no-relay \
  < "$WORK/a.in" > "$WORK/a.out" 2>&1 &
PIDS+=($!)

HOME="$HOME_B" TMPDIR="$TMP_B" "$REL/cc-connect" chat "$TICKET" --no-relay \
  < "$WORK/b.in" > "$WORK/b.out" 2>&1 &
PIDS+=($!)

# Hold the FIFOs open from this script so the chat REPLs don't see EOF.
exec 7> "$WORK/a.in"
exec 8> "$WORK/b.in"

# Wait until both peers print "Joined room:".
for _ in $(seq 1 60); do
  if grep -q 'Joined room:' "$WORK/a.out" 2>/dev/null \
     && grep -q 'Joined room:' "$WORK/b.out" 2>/dev/null; then
    break
  fi
  sleep 0.5
done
grep -q 'Joined room:' "$WORK/a.out" || { echo "FAIL: peer A never joined"; exit 1; }
grep -q 'Joined room:' "$WORK/b.out" || { echo "FAIL: peer B never joined"; exit 1; }
echo "[smoke] both peers joined the room"

# Give gossip a moment to mesh.
sleep 2

# ---------- 3. /drop a 4 KiB random payload from peer A ----------------------
TEST_FILE="$WORK/test-payload.bin"
head -c 4096 /dev/urandom > "$TEST_FILE"
EXPECTED=$(SHA "$TEST_FILE")
echo "[smoke] dropping $TEST_FILE  (sha256=${EXPECTED:0:16}…)"

echo "/drop $TEST_FILE" >&7

# ---------- 4. Wait for B's listener to fetch + export -----------------------
B_FILES_GLOB="$HOME_B/.cc-connect/rooms/*/files/*-test-payload.bin"

FOUND=""
for _ in $(seq 1 30); do
  for f in $B_FILES_GLOB; do
    [[ -f "$f" ]] && { FOUND="$f"; break; }
  done
  [[ -n "$FOUND" ]] && break
  sleep 1
done

if [[ -z "$FOUND" ]]; then
  echo "FAIL: peer B never received the dropped file"
  echo "--- B's rooms dir ---"
  find "$HOME_B/.cc-connect" -type f 2>/dev/null
  exit 1
fi

GOT=$(SHA "$FOUND")
if [[ "$GOT" != "$EXPECTED" ]]; then
  echo "FAIL: hash mismatch on B"
  echo "  expected: $EXPECTED"
  echo "  got:      $GOT"
  exit 1
fi

echo "[smoke] PASS: peer B exported $FOUND with matching sha256"

# ---------- 5. Verify the hook surfaces @file: on a fresh prompt -------------
HOOK_OUT=$(echo '{"session_id":"smoke","prompt":"hi"}' | \
  HOME="$HOME_B" TMPDIR="$TMP_B" "$REL/cc-connect-hook" 2>/dev/null || true)

if echo "$HOOK_OUT" | grep -qE '@file:.*test-payload\.bin'; then
  echo "[smoke] PASS: B's hook surfaces @file:<path> for the drop"
else
  echo "[smoke] WARN: hook did not surface @file: (output below)"
  echo "$HOOK_OUT" | head -10
fi

echo
echo "===== ALL OK ====="
