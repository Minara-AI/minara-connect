#!/usr/bin/env bash
# Local end-to-end smoke test for the persistent host-bg daemon.
#
# Boots `cc-connect host-bg start` (which detaches via setsid), then runs
# two `cc-connect chat` peers off the printed ticket — each in its own
# HOME so identities are independent — and verifies a /drop from peer A
# round-trips to peer B with byte-exact sha256, exactly like the
# foreground smoke test but with the daemon as the bootstrap node.
#
# Run with:  ./scripts/smoke-test-bg.sh
# Requires:  cargo build --workspace --release  has run.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REL="$REPO_ROOT/target/release"

if [[ ! -x "$REL/cc-connect" || ! -x "$REL/cc-connect-hook" ]]; then
  echo "missing binaries — run: cargo build --workspace --release" >&2
  exit 2
fi

if command -v sha256sum >/dev/null 2>&1; then
  SHA() { sha256sum "$1" | awk '{print $1}'; }
else
  SHA() { shasum -a 256 "$1" | awk '{print $1}'; }
fi

WORK=$(mktemp -d)
HOME_DAEMON="$WORK/daemon"
HOME_A="$WORK/peer-a"
HOME_B="$WORK/peer-b"
TMP_A="$WORK/tmp-a"
TMP_B="$WORK/tmp-b"
mkdir -p "$HOME_DAEMON" "$HOME_A" "$HOME_B" "$TMP_A" "$TMP_B"

DAEMON_PID=""
PEER_PIDS=()
TOPIC=""

cleanup() {
  rc=$?
  for p in "${PEER_PIDS[@]:-}"; do
    { kill "$p" 2>/dev/null && wait "$p" 2>/dev/null; } &
  done
  wait 2>/dev/null || true
  if [[ -n "$TOPIC" ]]; then
    HOME="$HOME_DAEMON" "$REL/cc-connect" host-bg stop "$TOPIC" >/dev/null 2>&1 || true
  fi
  if [[ $rc -ne 0 ]]; then
    echo
    echo "===== FAIL — leaving $WORK for inspection ====="
    for f in "$WORK"/start.out "$WORK"/a.out "$WORK"/b.out; do
      [[ -f "$f" ]] && { echo "--- $f ---"; tail -40 "$f"; }
    done
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

# ---------- 1. Start the daemon ---------------------------------------------
HOME="$HOME_DAEMON" "$REL/cc-connect" host-bg start > "$WORK/start.out" 2>&1
echo "[smoke-bg] daemon started:"
sed -n '1,10p' "$WORK/start.out" | sed 's/^/  | /'

TICKET=$(grep -m1 -oE 'cc1-[a-z0-9]+' "$WORK/start.out" || true)
[[ -n "$TICKET" ]] || { echo "FAIL: no ticket in start output"; exit 1; }
TOPIC=$(grep -oE 'host-bg stop [a-f0-9]+' "$WORK/start.out" | awk '{print $3}' | head -1)
[[ -n "$TOPIC" ]] || { echo "FAIL: no topic prefix in start output"; exit 1; }

# ---------- 2. List shows it ------------------------------------------------
LIST=$(HOME="$HOME_DAEMON" "$REL/cc-connect" host-bg list)
echo "[smoke-bg] list shows: $LIST"
echo "$LIST" | grep -q "$TOPIC" || { echo "FAIL: list missing topic"; exit 1; }

# ---------- 3. Two chat peers ------------------------------------------------
mkfifo "$WORK/a.in" "$WORK/b.in"
HOME="$HOME_A" TMPDIR="$TMP_A" "$REL/cc-connect" chat "$TICKET" \
  < "$WORK/a.in" > "$WORK/a.out" 2>&1 &
PEER_PIDS+=($!)
HOME="$HOME_B" TMPDIR="$TMP_B" "$REL/cc-connect" chat "$TICKET" \
  < "$WORK/b.in" > "$WORK/b.out" 2>&1 &
PEER_PIDS+=($!)

exec 7> "$WORK/a.in"
exec 8> "$WORK/b.in"

for _ in $(seq 1 90); do
  if grep -q 'Joined room:' "$WORK/a.out" 2>/dev/null \
     && grep -q 'Joined room:' "$WORK/b.out" 2>/dev/null; then
    break
  fi
  sleep 0.5
done
grep -q 'Joined room:' "$WORK/a.out" || { echo "FAIL: peer A never joined"; exit 1; }
grep -q 'Joined room:' "$WORK/b.out" || { echo "FAIL: peer B never joined"; exit 1; }
echo "[smoke-bg] both peers joined the daemon-hosted room"

sleep 2

# ---------- 4. /drop a 4 KiB file from A → B --------------------------------
TEST_FILE="$WORK/test-payload.bin"
head -c 4096 /dev/urandom > "$TEST_FILE"
EXPECTED=$(SHA "$TEST_FILE")
echo "[smoke-bg] dropping (sha256=${EXPECTED:0:16}…)"
echo "/drop $TEST_FILE" >&7

B_GLOB="$HOME_B/.cc-connect/rooms/*/files/*-test-payload.bin"
FOUND=""
for _ in $(seq 1 30); do
  for f in $B_GLOB; do
    [[ -f "$f" ]] && { FOUND="$f"; break; }
  done
  [[ -n "$FOUND" ]] && break
  sleep 1
done
[[ -n "$FOUND" ]] || { echo "FAIL: peer B never received the drop"; exit 1; }
GOT=$(SHA "$FOUND")
[[ "$GOT" == "$EXPECTED" ]] || { echo "FAIL: hash mismatch"; exit 1; }
echo "[smoke-bg] PASS: peer B exported $FOUND with matching sha256"

# ---------- 5. Stop the daemon ----------------------------------------------
HOME="$HOME_DAEMON" "$REL/cc-connect" host-bg stop "$TOPIC"
LIST=$(HOME="$HOME_DAEMON" "$REL/cc-connect" host-bg list)
echo "[smoke-bg] after stop, list: $LIST"
[[ "$LIST" == *"no daemons running"* ]] \
  || { echo "FAIL: list still shows a daemon"; exit 1; }
TOPIC=""  # already stopped, don't double-stop in cleanup

echo
echo "===== ALL OK ====="
