# cc-connect Protocol — v0.1 DRAFT

This document specifies the cc-connect wire protocol and on-disk layout. A second implementer reading this file should be able to write a client that interoperates with the reference implementation.

For terminology, see [`CONTEXT.md`](./CONTEXT.md). Decisions are recorded in [`docs/adr/`](./docs/adr/) — this spec does not re-litigate them.

The keywords **MUST**, **MUST NOT**, **SHOULD**, **MAY** follow [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119).

---

## 0. Status and versioning

- **Status:** DRAFT for v0.1. Wire format may break before v1.0.
- **Wire version:** every wire object carries a `"v": 1` field. Implementations **MUST** reject objects whose `v` is not understood. Implementations **SHOULD** tolerate (ignore) unknown JSON fields, to leave room for additive extensions.
- **Versioning policy:**
  - Bumping `v` indicates a breaking change to that object's shape.
  - ALPN strings include a major version (e.g. `cc-connect/v1/backfill`).
  - Adding a new optional field within an existing `v` is **not** a breaking change. Decoders **MUST** ignore unknown fields rather than rejecting the object. (Resolves the conflict between this section and §4 — MUST is the rule everywhere.)
  - **Precedence:** the `v` check is performed *first*. A Message with `v: 2` is rejected outright; the unknown-field-tolerance rule applies only after `v` is recognised.
- **iroh dependency pin (v0.1 only):** This protocol's Ticket and gossip wire formats inherit from the `iroh-gossip` crate. v0.1 reference implementation uses `iroh-gossip = 0.30.x` (latest patch on the 0.30 minor at the time of v0.1 tag). Cross-language second implementers either link the Rust crate via FFI, port the encoder, or constrain themselves to `cc-connect`-prefixed payloads (see §6.2) and accept that gossip interop requires the Rust reference today. v1.0 will replace this with a frozen byte-level spec.

---

## 1. Terminology

See [`CONTEXT.md`](./CONTEXT.md). Identifiers throughout this spec are case-sensitive unless explicitly noted.

---

## 2. Identity

Each Peer **MUST** hold exactly one Ed25519 keypair, persisted at `~/.cc-connect/identity.key` with file mode `0600`.

- File contents: 32 raw bytes — the Ed25519 secret-key seed. No header, no encoding, no padding.
- The same key **MUST** be passed to the iroh `Endpoint` so that the Peer's iroh `NodeId` equals the Peer's Pubkey. This makes transport-authenticated connections sufficient evidence of authorship in v0.1; see also §4.
- A Pubkey is the 32-byte Ed25519 public key.
- The canonical Pubkey string form is **lowercase RFC4648 base32 with no padding** of the 32 raw bytes (52 characters). This matches iroh's `NodeId` string encoding.
- On first run, an implementation **MUST** generate a new keypair if `identity.key` does not exist. It **MUST** create the file with mode `0600`. It **SHOULD** warn (and **MAY** refuse to load) if the file's mode has drifted to anything wider.

Rationale: ADR-0001.

---

## 3. Room ticket

A Ticket fully identifies a Room and provides bootstrap addresses for joining its gossip topic.

In v0.1, the Ticket bytes are the canonical serialization of an `iroh_gossip::Ticket` from the pinned crate version (§0):

```
ticket_bytes := iroh_gossip::Ticket::serialize()
              = topic_id (32 bytes) + bootstrap_addrs (iroh-gossip-internal encoding)
```

For implementer interop today, `iroh_gossip::Ticket` is documented in the iroh-gossip 0.30 crate. The serialised form for that pinned version begins with the 32-byte topic_id; the bootstrap-addr section is `postcard`-encoded (see iroh-gossip source). v1.0 of this protocol will inline a stable byte layout; v0.1 explicitly accepts the iroh-pin trade-off.

The user-facing **Room code** is:

```
room_code := "cc1-" + base32_alphabet_unpadded( ticket_bytes || CRC32_ISO_HDLC_be(ticket_bytes) )
```

Encodings pinned:
- **Base32 alphabet:** RFC 4648, lowercase, **no padding**. (Distinct from the Crockford alphabet used for ULIDs in §4 — do not interchange.)
- **CRC32 variant:** **CRC-32 / ISO-HDLC** (the same CRC used by zlib, RFC 1952, and gzip):
  - polynomial: `0xEDB88320` (reversed form of `0x04C11DB7`)
  - init: `0xFFFFFFFF`
  - reflect input: yes
  - reflect output: yes
  - xorout: `0xFFFFFFFF`
  - serialised: 4 bytes, **big-endian** (network byte order), appended after `ticket_bytes`.
- The `cc1-` prefix is mandatory ASCII, lowercase, exactly 4 characters.

Decoders **MUST**, in order:
1. Strip the `cc1-` prefix (case-sensitive lowercase match); reject otherwise (`INVALID_PREFIX`).
2. Decode RFC 4648 base32 (case-insensitive on input, but strict no-padding); reject otherwise (`BASE32_ERROR`).
3. Verify the trailing 4-byte CRC32 against the recomputed CRC of the leading bytes; reject otherwise (`CHECKSUM_MISMATCH`).
4. Pass the leading bytes (without CRC) to `iroh_gossip::Ticket::deserialize()`; propagate any error as `ROOM_CODE_DECODE_ERROR`.

Tickets **MAY** be transmitted over any out-of-band channel (Slack, paper, QR). They contain bootstrap node addresses; rotating them requires reissuing the Ticket.

A Room exists only as long as ≥1 Peer is participating in its gossip topic. After all Peers leave, the topic on the iroh network ceases; presenting the same Ticket later will create a new, isolated topic with the same id but no participants. Implementations **MUST NOT** assume Room state survives a "last Peer left" event.

---

## 4. Message schema

A Message is a JSON object. The reference shape:

```json
{
  "v": 1,
  "id": "01HZA8K9F0RS3JXG7QZ4N5VTBC",
  "author": "k3npfwj1y5wzcahmuxz66...",
  "ts": 1714323456789,
  "body": "use postgres for sessions"
}
```

| Field | Type | MUST/MAY | Notes |
|---|---|---|---|
| `v` | integer | MUST be `1` | Wire version |
| `id` | string | MUST | 26-character Crockford base32 ULID |
| `author` | string | MUST | Pubkey string form (see §2) |
| `ts` | integer | MUST | Unix milliseconds (UTC); informational only — see ordering rules below |
| `body` | string | MUST | UTF-8, max 8 KiB (8192 bytes after UTF-8 encoding, no Unicode normalisation imposed); senders **MUST** reject longer bodies, recipients **MUST** drop Messages whose `body` exceeds the cap |
| `kind` | string | MAY (default `"chat"`) | Reserved namespace; v0.1 senders **MUST NOT** emit values other than `"chat"`; v0.1 receivers **MUST** drop Messages with any other `kind` value (and **SHOULD** log the drop for diagnostics). Absence of `kind` is equivalent to `"chat"`. |

Implementations **MUST** ignore unknown top-level fields rather than rejecting the Message (forward-compat).

**JSON canonical encoding** (used for serialisation in log.jsonl, gossip broadcast, and the Backfill RPC bodies):
- UTF-8 only.
- Numeric `ts` **MUST** be a JSON integer literal (no exponent, no decimal point).
- String fields **MUST** use the minimal escape set required by RFC 8259 §7: `\"`, `\\`, `\b`, `\f`, `\n`, `\r`, `\t`, and `\u00xx` for ASCII control characters 0x00–0x1F not in the named set. Implementations **MUST NOT** emit `\/`, **MUST NOT** HTML-escape (`<`, `>`, `&` pass through), and **MUST NOT** use `\uXXXX` for any code point above 0x1F that has a UTF-8 representation.
- No insignificant whitespace inside the object, no trailing whitespace, no BOM.
- Field order in the canonical form: `v`, `id`, `author`, `ts`, `body`, then `kind` if present. Decoders **MUST NOT** rely on this order on input.

**Authorship trust (v0.1):** Messages are not signed. The `author` field is a self-claim. Receivers trust:
- For live gossip arrivals: iroh's QUIC/TLS authenticates the sending NodeId, which v0.1 binds to the author Pubkey via §2.
- For Backfill arrivals: receivers trust the responding Peer to forward Messages truthfully. There is no cryptographic guarantee that a Backfilled Message's `author` matches the original sender. v0.2 will add per-Message Ed25519 signatures.

Receivers **MUST** drop a Backfill response Message whose `author` equals the receiver's own Pubkey (self-spoof prevention). Receivers **SHOULD** log such drops.

For **gossip** arrivals, receivers **MUST** drop any Message whose `author` field does not equal the iroh `NodeId` of the publishing edge (the gossip event's authenticated sender). This binds wire-level transport authentication to application-level authorship.

This is a deliberate v0.1 simplification; document the threat model accordingly.

**ULID** (`id`):
- 26 characters of Crockford base32. Canonical output **MUST** be uppercase (per the ULID spec). Decoders **MUST** normalise per Crockford's character map (`I` and `i` and `l` and `L` → `1`; `O` and `o` → `0`; `U` and `u` → reject) before comparison and **MUST** treat upper- and lower-case as equal after normalisation. Two distinct strings that normalise to the same 128 bits are considered the same ULID for de-duplication and lex-ordering.
- Encodes a 48-bit Unix-ms timestamp + 80 bits of randomness.
- Lex order is the canonical Message ordering.

**Clock skew is not corrected in v0.1, with explicit consequences:**
- A Peer whose system clock is behind real time produces ULIDs that lex-sort earlier than current ULIDs. If a receiver has already advanced its Cursor (§9) past those IDs, the skewed Peer's Messages will be invisible to that Cursor — the Hook will never inject them into Claude.
- A Peer whose clock is ahead produces ULIDs that lex-sort later than concurrent senders, so its Messages always appear "newest" regardless of true wall-clock order.
- Implementations **SHOULD** rely on system NTP. Implementations **MAY** warn at startup if `cc-connect doctor` detects clock skew >5 minutes against an NTP probe.
- v0.2 will introduce a hybrid logical clock to bound this failure mode.

---

## 5. Chat log

The Chat log is a per-Room append-only file at:

```
~/.cc-connect/rooms/<topic_id_hex>/log.jsonl
```

- `<topic_id_hex>` is the 32-byte topic ID encoded as lowercase hex (64 characters).
- One Message per line. Each line **MUST** be valid JSON terminated by exactly one `\n` byte.
- File mode **MUST** be `0600` on creation.
- Append is the only supported operation. Implementations **MUST NOT** edit or delete prior lines in v0.1. Rotation/compaction is reserved for v0.2.

**Ordering:** Messages within a single Peer's local log appear in the order the Peer learned about them. Across different Peers, observers **MAY** see Messages in different physical order; lexicographic order by `id` is the canonical reading order. Implementations rendering chat **SHOULD** sort by `id` ascending before display.

**De-duplication:** When a Message arrives (gossip or Backfill), implementations **MUST**:
1. Check whether a Message with the same `id` already exists in the local log (typically by an in-memory set or seek-from-tail scan).
2. If yes: drop the new arrival silently.
3. If no: append.

If two Messages with the same `id` but different content arrive, implementations **MUST** keep the first observed and discard later arrivals.

**Corruption tolerance:** When reading the log, implementations **SHOULD** skip and warn on any line that fails to parse, continue with the next, and never abort.

**Concurrent-writer atomicity:** A Peer may write to log.jsonl from two paths concurrently — the chat REPL appending the local user's outgoing Message, and the gossip listener appending an incoming Message. Both writers **MUST** serialise via an `flock(LOCK_EX)` on the log file (acquired before write, released after fsync). POSIX `O_APPEND` write atomicity is only guaranteed up to `PIPE_BUF` (typically 4096 bytes) — Messages near the 8 KiB cap can interleave without the lock. The same lock is held by `cc-connect-hook` readers in §7.3; readers acquire `LOCK_SH` to permit concurrent reads but block writers.

The parent directory `~/.cc-connect/rooms/<topic_id_hex>/` **MUST** be created with mode `0700` if missing.

---

## 6. Transport

cc-connect runs on a single iroh `Endpoint` per Peer. Multiple protocols are multiplexed by ALPN:

| ALPN | Protocol | Section |
|---|---|---|
| `iroh-gossip/0` | Real-time Message broadcast (set by `iroh-gossip` 0.30; see §0 pin) | §6.1 |
| `cc-connect/v1/backfill` | One-shot history fetch on join | §6.2 |
| `iroh-blobs/...` | Blob transfer | reserved for v0.2 |

### 6.1 Gossip topic

- The Room's iroh-gossip topic is the 32-byte topic_id from the Ticket (§3).
- A Peer joins by calling `iroh_gossip::Gossip::join(topic, bootstrap_node_ids)` after starting its `Endpoint`. The gossip ALPN is set by the iroh-gossip crate; cc-connect does not override it.
- **Message framing on the gossip transport:** each Message is published as a single `Gossip::broadcast` call carrying the bytes of one canonically-encoded JSON Message (§4). One Message per gossip event; **no length prefix and no terminator beyond the gossip frame itself.** Concatenated multi-Message payloads are not permitted; receivers seeing extra trailing bytes after the JSON object **MUST** drop the event and warn.
- To send a Message, a Peer **MUST**:
  1. Construct the Message JSON object per §4 (`v=1`, fresh ULID, own Pubkey, current Unix ms, body).
  2. Serialize to UTF-8 bytes per the canonical encoding (§4).
  3. Acquire log.jsonl `flock(LOCK_EX)`, append the line + `\n`, fsync, release.
  4. Publish via `Gossip::broadcast(topic, message_bytes)`.
- On receipt of a gossip event, the receiving Peer **MUST**:
  1. Deserialize as JSON; on parse error, drop and warn.
  2. Validate `v == 1` and all required fields per §4; on failure, drop and warn.
  3. De-duplicate by `id` (see §5).
  4. Acquire log.jsonl `flock(LOCK_EX)`, append, fsync, release.

### 6.2 Backfill RPC

When a Peer joins a Room **with at least one other Peer already present**, it **MUST** request Backfill from one already-online Peer. (A Host invoking `cc-connect host` for a brand-new Room has no Peer to ask; it skips Backfill, marks the Room ready immediately, and proceeds to active-rooms registration in §8.) Backfill rides a direct `iroh-net` connection with ALPN `cc-connect/v1/backfill`.

A joiner detects "no other Peer present" by waiting up to 1 second after gossip join for any presence signal from another participant (e.g. a gossip neighbour announcement); if none arrives, the joiner treats itself as the lone Peer and skips Backfill.

**Connection:** the joiner picks a known online Peer (e.g. the Peer whose `NodeAddr` it just received from gossip presence) and dials it with the Backfill ALPN.

**Wire format on the open stream:**
- Both request and response are **4-byte big-endian length prefix (in bytes)** + UTF-8 JSON body of exactly that length.
- **Maximum length:** `1 << 24` bytes (16 MiB) per frame. Both sides **MUST** reject larger length values without reading the body (anti-DoS).
- One request, one response, then the responder **MUST** close the stream.
- Both sides **MUST** validate that the JSON body's `v == 1`; reject otherwise.

**Request:**
```json
{
  "v": 1,
  "since": "01HZA8K9F0RS3JXG7QZ4N5VTBC",
  "limit": 50
}
```
- `since`: exclusive lower bound by ULID. **MAY** be `null` to mean "send your latest `limit` Messages."
- `limit`: integer, capped at 50 in v0.1. Servers **MUST** clamp larger values down.

**Response:**
```json
{
  "v": 1,
  "messages": [<Message>, <Message>, ...]
}
```
- `messages`: array, ordered ascending by `id`. **MAY** be empty if the responder has no Messages newer than `since`. The responder **MUST** exclude any Message with `id == since` (`since` is exclusive). The responder **MUST** return all matching Messages up to `limit`, even when fewer than `limit` qualify (no padding, no upper-bound truncation other than `limit`).

**Timeout:** the joiner **MUST** abandon a Backfill that has not produced a complete response within 5 seconds of stream open.

**Joiner behaviour on timeout / no responder:**
1. Try the next online Peer at random (server-side state is per-call; rerolling is safe).
2. If no Backfill has succeeded after **10 seconds aggregate** from the first attempt, the joiner **MUST** abort any in-flight Backfill (cancel the iroh stream), surface a marker line in the chat REPL: `[chatroom] (joined late, no history available)`, and proceed. The 10-second aggregate is a hard cap regardless of how many peers were attempted.

**Responder behaviour:** any Peer with the Backfill ALPN registered on its endpoint **MUST** either send a complete response or close the stream cleanly within 5 seconds. Silent hangs are non-conformant. A responder with an empty local log returns `{"v":1,"messages":[]}` rather than refusing.

Rationale: ADR-0002.

---

## 7. Hook contract

The Hook is the `cc-connect-hook` binary, invoked by Claude Code on `UserPromptSubmit`. It is the v0.1 bridge that turns Substrate state into Claude Context.

### 7.1 Configuration

A user enables the Hook by adding the following to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "type": "command",
        "command": "/absolute/path/to/cc-connect-hook"
      }
    ]
  }
}
```

The path **SHOULD** be absolute. Bare-name commands (e.g. `cc-connect-hook`) silently fail when Claude Code's process `PATH` does not include the binary's install location (commonly `~/.cargo/bin` or `~/.local/bin`); `cc-connect doctor` warns in that case. See the design doc's pre-v0.1 prep for the install ergonomics rationale.

### 7.2 Input

The Hook reads exactly one JSON object from stdin (Claude Code-supplied). The fields it consumes:

| Field | Type | Required | Use |
|---|---|---|---|
| `session_id` | string | yes | Identifies the Claude Code Session for cursor namespacing |

If the Hook cannot parse stdin or `session_id` is missing, it **MUST** write a one-line warning to stderr, emit nothing on stdout, and exit `0`. The user's prompt **MUST NOT** be blocked on Hook errors.

### 7.3 Operation

In order:

1. Determine `$UID` and `$TMPDIR` (defaulting `TMPDIR` to `/tmp`). Construct the active-rooms directory: `${TMPDIR}/cc-connect-${UID}/active-rooms/`.
2. List `*.active` files in that directory. For each:
   - Read the file contents (one line, integer PID).
   - Call `kill(pid, 0)`. If the call fails with `ESRCH` (no such process), unlink the file and skip this room.
   - Otherwise treat the room as active; the filename's stem (without `.active`) is the topic_id_hex.
3. For each active Room, in any order:
   - Open the cursor file at `~/.cc-connect/cursors/<topic_id_hex>/<session_id>.cursor`. Create parent dirs with mode `0700` if needed. Open with `O_RDWR | O_CREAT`, mode `0600`.
   - Acquire `flock(LOCK_EX)` on the file descriptor. Hold until step 8.
   - Read the cursor: empty file → `cursor = null`; otherwise the content is interpreted per §9 (a bare ULID for v0.1; future-tolerant of JSON).
4. Open `~/.cc-connect/rooms/<topic_id_hex>/log.jsonl` read-only with `flock(LOCK_SH)`. Linear scan forward from the start, parsing each line as a Message; skip and warn on any malformed line. Collect Messages whose `id > cursor` (lex compare). Linear scan is the v0.1 strategy; v0.2 adds a `byte_offset` field to the Cursor (see TODOS.md) to avoid full-file rescans on large logs.
5. Format each kept Message:
   - **Single Room active:** `[chatroom @<nick> <hh:mm>Z] <body>\n`
   - **Multiple Rooms active:** `[chatroom <room-tag> @<nick> <hh:mm>Z] <body>\n` where `<room-tag>` is the first 6 characters of the lowercase hex topic_id (e.g. `a1b2c3`). The tag is for human and Claude differentiation; it has no protocol semantics.
   - `<nick>` is the value mapped from the Message's `author` Pubkey by `~/.cc-connect/nicknames.json` (a flat `pubkey → nickname` JSON object), falling back to the first 8 characters of the Pubkey. Nicknames containing `\n`, `\r`, `\t`, or characters outside the printable ASCII range `0x20–0x7E` **MUST** be replaced byte-for-byte with `?`.
   - `<hh:mm>Z` is the **UTC** hour:minute derived from `ts` (zero-padded, 24-hour, trailing `Z` to mark UTC). Local-time rendering is reserved for the chat REPL display, not the Hook output.
   - `<body>` is the Message body with all bytes in the C0 control range (`0x00–0x1F`) plus `0x7F` (DEL) replaced by single ASCII spaces for single-line emission. UTF-8 multi-byte sequences are preserved.
6. If the cumulative formatted output across **all** Rooms would exceed 8 KiB (8192 bytes after UTF-8 encoding, including all newlines and any prepended marker line):
   - Drop the **oldest** Messages (by `id`, across the merged-and-sorted multi-Room set) until the remaining set plus the marker line fits.
   - Prepend a single line: `[chatroom] (N older messages skipped to fit)\n` where `N` is the count actually dropped.
   - The fit check is iterative — adding the marker line itself can change the budget; implementations **MUST** loop until the final size is ≤ 8 KiB.
   - The 8 KiB cap is hard. Rationale: ADR-0004.
7. Write the assembled output to stdout in chronological order across all active Rooms — interleaved by Message `id` ascending. Each output line carries its `<room-tag>` (multi-room case) so Claude can distinguish.
8. For each cursor file held: write the highest considered Message `id` (including any dropped in step 6) to a sibling `.tmp` file, fsync the `.tmp` file, then `rename(2)` over the cursor file, then fsync the parent directory. Release the flock.

   **flock-vs-rename race note:** an `flock` is held against the open file descriptor's inode, not against the path. After step 8's `rename(2)` replaces the path with a new inode, any other Hook invocation that opened the path **before** the rename still holds a lock on the *old* inode and is invisible to subsequent invocations. To avoid lost cursor advances, implementations **MUST** use the following discipline: (a) open with `O_RDWR | O_CREAT`; (b) acquire `flock(LOCK_EX)`; (c) **after** acquiring the lock, `stat` the path and the held fd's inode (`fstat`); if they differ, close, re-open, and retry the lock acquisition (bounded loop, e.g. 5 attempts before bailing); (d) only then read/modify/rename. This guarantees the rename winner's `LOCK_EX` is the one that actually serializes the next reader.
9. Exit `0`.

### 7.4 Error semantics

- All non-zero exit codes block the user prompt in Claude Code. The Hook **MUST** always exit `0` regardless of internal errors. The only exceptions are unrecoverable runtime initialisation failures before main() runs (e.g. memory allocator panic). Within main(), any error path **MUST** log to `~/.cc-connect/hook.log` (mode `0600`), emit nothing on stdout, and exit `0`.
- The Hook **MUST** unify on `fcntl(F_SETLK)` byte-range locking (NFS-safe) rather than `flock(2)` for cursor and log files. v0.1 explicitly does not support `~/.cc-connect/` mounted on NFS or any non-POSIX filesystem; if `cc-connect doctor` detects NFS, it **SHOULD** warn.
- Empty stdout + exit `0` is the canonical "no new Messages" outcome.
- If the flock-vs-rename retry in step 8 exhausts its attempt budget, the Hook **MUST** skip that Room (do not write its cursor, do not include its messages in stdout), log to `hook.log`, and continue with other Rooms.

### 7.5 Overflow above 8 KiB

In normal operation the 8 KiB cap is enforced in step 6. If a future bug causes the Hook to emit more than 8 KiB:
- Claude Code persists the full stdout to a file under `~/.claude/projects/<project>/<session>/tool-results/hook-<uuid>-stdout.txt` and injects a ~2 KiB inline preview plus a `<persisted-output>` system-reminder pointing to the file.
- Claude **MAY** read the persisted file via its `Read` tool to recover the full payload.

This is the graceful overflow path documented in ADR-0004 and verified by Spike 0. It is a safety net, not a designed feature.

---

## 8. Active-rooms protocol

A `cc-connect chat` process advertises its active Room by writing a marker file:

```
${TMPDIR}/cc-connect-${UID}/active-rooms/<topic_id_hex>.active
```

- `<topic_id_hex>` is the lowercase hex of the 32-byte topic ID.
- File contents: a single line containing the integer PID of the `cc-connect chat` process, in ASCII decimal. No trailing newline is required; readers **MUST** tolerate either presence or absence of a trailing `\n`. Readers **MUST** validate that the parsed value is a positive integer in the range `[100, 2^31 - 1]` (rejecting `0`, `1`, negatives, oversized, and non-numeric content); failed validation results in unlinking the file and skipping the Room. The `≥ 100` floor avoids false-active matches against PID 0 (kernel) and PID 1 (init); the upper bound covers Linux's max-pid range.
- File mode: `0600`. Parent directory mode: `0700`. Implementations **MUST** `lstat` the parent directory and refuse if (a) it is a symlink, (b) it is not a directory, or (c) its mode is not exactly `0700`. A hostile co-tenant could otherwise pre-create the path with looser permissions or as a symlink to a snoopable location. The canonical recovery is `rm -rf "$TMPDIR/cc-connect-$UID/" && cc-connect chat <ticket>`.

**Lifecycle:**
- The `cc-connect chat` process **MUST** create the file *after* gossip join and Backfill have completed, not during bootstrap. This prevents Hook fires from injecting from a not-yet-ready Room.
- The process **SHOULD** install an `atexit` handler and signal traps for `SIGTERM`, `SIGINT`, `SIGHUP` to unlink the file on clean exit. SIGKILL leaves the file; that is acceptable.
- The Hook (§7) is the canonical sweeper for stale entries, via `kill(pid, 0)` checks.

Placing the directory under `$TMPDIR` (per-machine) rather than under `~` (potentially sync-replicated) avoids PIDs from one machine being misinterpreted on another. Rationale: ADR-0003.

---

## 9. Cursor format

A Cursor records the highest Message `id` already injected into a particular Claude Code Session for a particular Room.

- Path: `~/.cc-connect/cursors/<topic_id_hex>/<session_id>.cursor`
- File mode: `0600`. Parent directory mode: `0700`.
- v0.1 content: a single line containing one ULID. An empty file or a missing file means "no Messages yet seen."

For forward compatibility, readers **MUST** tolerate a JSON-object form:

```json
{ "v": 1, "id": "01HZA8K9F0RS3JXG7QZ4N5VTBC" }
```

If the first non-whitespace byte is `{`, parse as JSON and read the `id` field; otherwise treat the trimmed content as a bare ULID. v0.1 writers emit only the bare-ULID form. v0.2 may add `byte_offset` and other fields (see TODOS.md).

Updates **MUST** be atomic: write the new value to a sibling `.cursor.tmp` file, fsync, then `rename(2)` over the canonical cursor file while holding the flock from §7.

---

## 10. Reserved namespaces

The following names are reserved and **MUST NOT** be used by v0.1 implementations except as specified:

- **Message kinds:** `chat` (default in v0.1), `file_drop` (v0.2+), `system` (v0.2+).
- **URI scheme:** `cc://` is reserved for MCP resource URIs in v0.2+. v0.1 implementations **MUST NOT** register handlers or expose resources under this scheme. Message bodies **MAY** contain `cc://` strings as data; the reservation applies only to URI handler registration and resource publication.
- **ALPN strings:** any ALPN beginning with `cc-connect/` is reserved for the cc-connect protocol family. v0.1 uses `cc-connect/v1/backfill`. Future ALPNs (e.g. `cc-connect/v1/file-drop`) follow the same naming.
- **Filesystem paths:** `~/.cc-connect/`, `${TMPDIR}/cc-connect-${UID}/`, `~/.claude/settings.json`'s `hooks.UserPromptSubmit` array.

---

## 11. Conformance test vectors

These vectors **MUST** match byte-for-byte across implementations. The reference implementation will publish a `tests/vectors/` directory with each vector as a fixture file; vectors below describe the canonical inputs and outputs in enough detail that an implementer can reproduce the bytes from first principles.

### 11.1 Pubkey encoding

**Input:** Ed25519 secret-key seed = 32 bytes of `0x00`.

**Derived public key** (32 bytes, hex):
```
3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29
```

**Canonical Pubkey string** (RFC 4648 base32, lowercase, no padding, of the 32-byte public key):
```
hnvcppgow2sc2yvdvdicu3ynonsteflxdxrehjr2ybekdc2z3iuq
```

Implementations **MUST** produce this exact 52-character string when encoding the test pubkey above.

### 11.2 Message JSON canonical encoding

**Input fields:**
```
v       = 1
id      = 01HZA8K9F0RS3JXG7QZ4N5VTBC      (Crockford base32, uppercase)
author  = hnvcppgow2sc2yvdvdicu3ynonsteflxdxrehjr2ybekdc2z3iuq   (the §11.1 pubkey)
ts      = 1714323456789                  (Unix milliseconds, integer literal)
body    = use postgres                    (12 bytes UTF-8, no escapes needed)
```

**Canonical output** (single line, UTF-8, exactly the bytes shown — no leading/trailing whitespace; the line terminator `\n` is added separately by log.jsonl appenders):

```
{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"hnvcppgow2sc2yvdvdicu3ynonsteflxdxrehjr2ybekdc2z3iuq","ts":1714323456789,"body":"use postgres"}
```

Length: 146 bytes (verified by `cc-connect-core::message::tests::protocol_11_2_canonical_encoding_byte_exact`). Field order **MUST** match the canonical form exactly when emitting. Decoders **MUST NOT** require this ordering on input.

**Edge-case body vector:** with `body = "<é>\n\"x"` (literal: `<`, `é` as 0xc3 0xa9, `>`, raw newline 0x0a, `"`, `x` — 7 input bytes UTF-8), the canonical encoding of the `body` JSON string value (just the `"…"` field value, not including `"body":`) is:
```
"<é>\n\"x"
```
That is: `<` and `>` pass through unescaped, `é` is emitted as its raw UTF-8 bytes (`0xc3 0xa9`), the newline is escaped as `\n` (two ASCII bytes), and the embedded quote is escaped as `\"`. No `\u` escapes for code points ≥ 0x20. Total bytes (including the surrounding quotes): **11**.

### 11.3 Backfill request wire bytes

**Input:** request `{ v: 1, since: null, limit: 50 }`.

**JSON canonical body** (28 bytes):
```
{"v":1,"since":null,"limit":50}
```

**Full on-stream bytes** (32 bytes, hex; 4-byte BE length prefix + JSON):
```
00 00 00 1f 7b 22 76 22 3a 31 2c 22 73 69 6e 63
65 22 3a 6e 75 6c 6c 2c 22 6c 69 6d 69 74 22 3a
35 30 7d
```

(Length 0x0000001f = 31 bytes JSON body. Earlier draft of this vector miscounted — the canonical request including no extra whitespace is **31 bytes**: `{"v":1,"since":null,"limit":50}`. Implementations **MUST** emit exactly this byte sequence.)

### 11.4 Hook stdout from a known log

**Inputs:**
- One active Room (so the single-Room format from §7.3 step 5 applies, no `<room-tag>`).
- log.jsonl contents (3 Messages, listed here as the canonical 11.2-style JSON, one per line — substitute the §11.1 pubkeys A and B as authors):
  ```
  Message M1: ts=1714000000000, author=A, body="hello"
  Message M2: ts=1714000060000, author=B, body="reply"
  Message M3: ts=1714000120000, author=A, body="ack"
  ```
  (concrete ULIDs and full canonical JSON for each are published in `tests/vectors/log-3msgs.jsonl` once the reference impl lands; implementations **MUST** verify against it then.)
- nicknames.json: `{"A_pubkey": "alice", "B_pubkey": "bob"}`.
- Cursor file content: `M1`'s ULID (so M1 is already seen).
- session_id: `test-session-001`.

**Expected stdout** (UTF-8, exactly these bytes, with `\n` line terminators):
```
[chatroom @bob 00:01Z] reply
[chatroom @alice 00:02Z] ack
```

Note: `00:01Z` and `00:02Z` are derived from `ts` by `(ts / 60000) % 1440` formatted as `HH:MM` UTC. (1714000060000 ms → 28566667 minutes → 28566667 mod 1440 = 1 minute past midnight UTC → `00:01Z`.)

After the Hook completes, the cursor file **MUST** atomically contain `M3`'s ULID.

### 11.5 Hook truncation

**Inputs:** a log of 100 Messages each with body of 100 ASCII bytes (so each formatted line is ~140 bytes; total ~14 KiB > 8 KiB cap). Cursor = null.

**Expected behaviour:**
- The Hook keeps as many *newest* Messages as fit under 8192 bytes including the marker line.
- Empirically (140 bytes per line × N + marker line ~50 bytes ≤ 8192): N ≈ 58. Implementations **MUST** publish their concrete N for this fixture once the reference impl exists.
- Output starts with `[chatroom] (M older messages skipped to fit)\n` where M = 100 − N.
- Cursor advances to the highest-id Message in the entire 100-Message set, *not* just the kept ones.

### 11.6 Stale-PID sweep

**Inputs:** `${TMPDIR}/cc-connect-${UID}/active-rooms/` contains:
- `aaa....active` containing the PID of the currently-running test process (alive)
- `bbb....active` containing PID `99999` (assumed dead in the test environment)

**Expected behaviour after `cc-connect-hook` runs:**
- `aaa.active` still present.
- `bbb.active` unlinked.
- Hook stdout (assuming `aaa` Room has no unread Messages): empty.
- Exit code: 0.

### 11.7 Cursor format compatibility

**Input cursor file containing exactly:**
```
01HZA8K9F0RS3JXG7QZ4N5VTBC
```
(no trailing whitespace, no JSON braces.) **Expected:** parsed as bare-ULID form per §9. Cursor value = `01HZA8K9F0RS3JXG7QZ4N5VTBC`.

**Input cursor file containing exactly:**
```
{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC"}
```
**Expected:** parsed as JSON form per §9 forward-compat. Cursor value = `01HZA8K9F0RS3JXG7QZ4N5VTBC`.

Both must produce the same cursor value. Implementations **MUST** support reading both; v0.1 writers emit only the bare-ULID form.

---

## Open items deferred to v0.2+

- Per-Message Ed25519 signatures (verifiable authorship across forwarding).
- Cursor `byte_offset` extension (large-log scan optimization).
- `cc://` MCP resource scheme (richer query patterns).
- `file_drop` Message kind + iroh-blobs integration.
- `system` Message kind (e.g. for Claude-as-peer in v0.3+).
- E2E content encryption with ticket-derived keys.
- Multi-Room per-Session subscription filtering.

These are not required for a conformant v0.1 implementation.

---

## Known limitations of v0.1 (acknowledged by spec author)

These are real gaps that a second implementer should know about up front. They are not fixed in v0.1 by deliberate scope choice; v1.0 will close them.

1. **Ticket bytes are not inlined.** §3 references `iroh_gossip::Ticket` serialisation rather than spelling out the postcard-encoded layout. A non-Rust client cannot reach gossip interop today without porting iroh-gossip's encoder. This is a real blocker for clean-room non-Rust implementations.
2. **Presence detection uses iroh-gossip events.** §6.2's "1-second wait for presence" is implemented in the reference client by listening for `iroh_gossip::Event::NeighborUp`. A non-Rust client must port that event surface or define a cc-connect-layer presence ping in v0.2.
3. **PID-based liveness is not foolproof.** §8's `kill(pid, 0)` cannot distinguish "still our process" from "PID was reused by an unrelated program after a reboot." The window is small in practice (PIDs are not reused frequently within a session) but real. v0.2 adds a process-name check via `/proc/<pid>/comm` (Linux) or `proc_pidpath` (macOS).
4. **Conformance vectors §11.4–§11.5 are partial.** ULIDs, Pubkey strings for authors A and B, the truncation count `N`, and the full canonical JSON for each Message in the 3-message log are listed as "published with the reference impl v0.1 release." A second implementer cannot byte-for-byte verify those vectors before the reference impl exists. v0.1.1 (post-impl) will inline them.
5. **No depth/size cap on `messages[]` in Backfill responses.** A malicious responder could send `{"v":1,"messages":[<a million 1-byte JSON objects>]}` within the 16 MiB stream cap. Receivers **SHOULD** apply a sensible per-Message-count cap (e.g. 10× the requested `limit`) but v0.1 does not require a specific value.
6. **No Unicode normalisation.** Two bodies that look identical to a human (NFC vs NFD form) are distinct on the wire and may de-duplicate inconsistently across clients with different default normalisation. Implementations **MAY** normalise to NFC on emit; v0.1 does not require it.
7. **Time rendering in Hook output is day-collision-prone.** §7.3 step 5's `<hh:mm>Z` format does not include the date. A Message from yesterday and a Message from today both render as the same `HH:MMZ` if their wall-clock minute matches. Acceptable for v0.1's "ambient awareness for current session" framing; v0.2 may add a date prefix when crossing midnight UTC.
8. **`fcntl` byte-range locking on the same inode is required across read and write paths.** Implementations **MUST** verify their language standard library uses `fcntl(F_SETLK)` rather than `flock(2)`; some Rust file-locking crates default to `flock`. The protocol itself is locking-strategy-agnostic, but mixed strategies on the same file produce silent corruption.

## Cross-references

- [`CONTEXT.md`](./CONTEXT.md) — domain glossary
- [`docs/adr/0001-machine-scoped-identity.md`](./docs/adr/0001-machine-scoped-identity.md) — Identity is per-machine
- [`docs/adr/0002-backfill-via-custom-rpc-not-iroh-docs.md`](./docs/adr/0002-backfill-via-custom-rpc-not-iroh-docs.md) — Backfill design
- [`docs/adr/0003-pid-based-active-rooms-discovery.md`](./docs/adr/0003-pid-based-active-rooms-discovery.md) — IPC + cursor lock
- [`docs/adr/0004-hook-budget-and-graceful-overflow.md`](./docs/adr/0004-hook-budget-and-graceful-overflow.md) — Hook budget
- [`spike/RESULTS.md`](./spike/RESULTS.md) — Spike 0 evidence
- [`TODOS.md`](./TODOS.md) — deferred items
