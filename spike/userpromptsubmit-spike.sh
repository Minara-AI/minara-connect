#!/usr/bin/env bash
# Spike 0 — UserPromptSubmit byte-cap probe for Claude Code (cc-connect v0.1).
#
# Emits a known-pattern blob of N KB to stdout. Each line is exactly 80 bytes
# (incl. newline) and tagged with an incrementing position marker (POS-NNNNN).
# Begin/end sentinel lines bound the blob so a Claude session can identify
# precisely how much of the injection it received.
#
# Set CC_SPIKE_SIZE_KB to control the payload size. Defaults to 1.
#
# This script is throwaway. The result of running it is what feeds back into
# the hook contract in PROTOCOL.md and ADR-TBD on Spike 0 outcome.

set -euo pipefail

SIZE_KB="${CC_SPIKE_SIZE_KB:-1}"
BYTES_PER_LINE=80   # "POS-NNNNN-" (10) + 69 X chars + newline (1)
TOTAL_BYTES=$(( SIZE_KB * 1024 ))
LINES=$(( TOTAL_BYTES / BYTES_PER_LINE ))

printf '<<<SPIKE-BEGIN size_kb=%s expected_lines=%s expected_bytes=%s>>>\n' \
  "$SIZE_KB" "$LINES" "$TOTAL_BYTES"

XPAD='XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX'
for i in $(seq 1 "$LINES"); do
  printf 'POS-%05d-%s\n' "$i" "$XPAD"
done

printf '<<<SPIKE-END last_line=POS-%05d>>>\n' "$LINES"
exit 0
