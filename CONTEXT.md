# cc-connect

A protocol and reference implementation for sharing chat-and-files context across multiple, independently-running Claude Code instances over a peer-to-peer network. Each developer keeps their own Claude; the project synchronises the *substrate* — what they're saying and what they've dropped on the table — so each Claude has the same input.

## Language

**Room**:
A named, ephemeral context where Peers share a Substrate.
_Avoid_: channel, session, group, topic.

**Ticket**:
A self-contained byte string that fully identifies a Room and provides bootstrap addresses; produced by the Host, copy-pasted out-of-band to invite.
_Avoid_: invite, link, token.

**Room code**:
A short, human-shareable encoding of a Ticket. Decodes losslessly back to a Ticket.
_Avoid_: code, shortcode, room id.

**Substrate**:
The shared, append-only contents of a Room — Messages and (later) Blobs. The thing each Peer's Claude reads.
_Avoid_: shared state, history, context store.

**Peer**:
A running cc-connect process on one machine that participates in a Room.
_Avoid_: client, node, member.

**Host**:
The Peer that creates a Room (originates the Ticket). Has no special privilege after creation.
_Avoid_: owner, admin, server.

**Identity**:
A keypair representing one machine. v0.1 uses it to authenticate the iroh transport (the Pubkey is the iroh NodeId); v0.2+ also signs each Message with it.
_Avoid_: account, login, user.

**Pubkey**:
The public half of an Identity. The canonical machine-readable handle for a Peer in the Substrate.
_Avoid_: id, address, key.

**Nickname**:
A client-local, human-readable label mapped to a Pubkey. Not part of the protocol — each Peer maintains its own.
_Avoid_: username, display name, alias.

**Message**:
A single Substrate entry authored by exactly one Identity. Append-only, never edited.
_Avoid_: post, entry, line, event.

**Chat log**:
The ordered Message stream of a Room.
_Avoid_: history, transcript, feed.

**Backfill**:
The Messages a newly-joined Peer receives from any already-connected Peer to catch up on Room history that pre-dates the join. v0.1 caps Backfill at the last 50 Messages.
_Avoid_: catchup, sync, replay.

**Blob** (v0.2+):
Content-addressed binary attached to a Room. Referenced from a `file_drop` Message.
_Avoid_: file, attachment, asset.

**Session**:
A single Claude Code conversation. The atomic unit a Cursor advances against.
_Avoid_: conversation, thread, claude.

**Hook**:
The cc-connect bridge that runs on every prompt and copies unread Messages from the Substrate into the next prompt's Context.
_Avoid_: injector, plugin.

**Cursor**:
A marker per (Room, Session) recording the highest Message id already injected into that Session.
_Avoid_: offset, watermark, position.

**Injection**:
The act of the Hook emitting unread Messages, which Claude Code splices into the prompt Context.
_Avoid_: inject, push.

**Context**:
The prompt content Claude sees on a turn. Includes Injection output. Substrate flows into Context but Substrate ≠ Context.
_Avoid_: prompt, history, memory.

**Claude PID Binding** (v0.6+):
The trust-boundary mechanism that ties one running Claude Code process to the Rooms its Peer has joined. The Hook and the MCP server each walk their parent process chain to find the `claude` ancestor's PID; that PID keys `~/.cc-connect/sessions/by-claude-pid/<pid>/rooms.json`. Replaces the pre-v0.6 `CC_CONNECT_ROOM` environment variable.
_Avoid_: room env, room var, claude topic.

**Consent gate** (v0.6+):
The mandatory human approval step between `cc_join_room` (Claude requests a Room binding) and the Room actually appearing in that Claude's rooms.json. Defends against in-Room prompt-injection from coercing Claude into subscribing to a hostile Room.
_Avoid_: handshake, approval.

**Pending join** (v0.6+):
A `cc_join_room` request awaiting the Consent gate. Persists at `~/.cc-connect/pending-joins/<token>.json` until either `cc-connect accept <token>` consumes it (binding the Room) or the human deletes it.
_Avoid_: pending invite, pending request.

## Design intent (canonical phrasings)

**Ambient awareness**:
The product feeling: each Peer's Claude silently knows what's happening in the Room, without explicit @mention or copy-paste.

**Chat-as-substrate**:
The architectural framing: chat history is not a side channel, it *is* the shared context.

**Parallel Claudes**:
The architectural framing: every Peer runs their own Claude Code locally; cc-connect does not multiplex one Claude across humans.

**Magic moment**:
The single demo gesture: Bob types in chat, Alice's next prompt has it, Alice did nothing. The v0.1 success criterion.

**Shared cursor** *(rejected framing — use only when explaining what cc-connect is NOT)*:
Architecture where multiple humans control one Claude Session, à la Live Share for AI.

## Relationships

- A **Host** creates exactly one **Room** per `cc-connect host` invocation; the Room exists as long as ≥1 Peer is online.
- A **Room** has exactly one **Ticket**; a **Room code** decodes losslessly to a **Ticket**.
- A **Peer** holds exactly one **Identity** (one **Pubkey**).
- A **Peer** authors zero or more **Messages**; every **Message** has exactly one **Pubkey** as its author.
- A **Session** has exactly one **Cursor** *per Room it observes*.
- A **Hook** invocation reads exactly one **Cursor** and advances it on success.
- A **Substrate** is the union of the **Chat log** (always) and **Blobs** (v0.2+).
- A **Peer** joining a **Room** receives a **Backfill** from any one already-connected Peer; multiple Peers may respond and the joiner deduplicates by Message id.

## Example dialogue

> **Dev:** "When Alice's Claude finishes a turn, where does Bob's last **Message** show up?"

> **Domain expert:** "The **Hook** runs on her next prompt, reads her **Cursor** for that **Room** + **Session**, finds **Messages** newer than the Cursor, and emits them as part of the **Injection**. Claude Code splices that into her prompt's **Context**."

> **Dev:** "If Alice has two Claude tabs open in the same Room, does Bob's Message show up in both?"

> **Domain expert:** "Yes — each tab is a distinct **Session** with its own **Cursor** for that Room. Each Session sees Bob's Message exactly once, the first time it submits a prompt after the Message arrived."

> **Dev:** "What if Alice's machine never received Bob's Message because she was offline?"

> **Domain expert:** "Then there's nothing in her local **Chat log** to inject. When she reconnects, her Peer catches up. The Cursor only advances over Messages that landed on her machine."

> **Dev:** "And the **Magic moment** is when this happens with no manual gesture from Alice?"

> **Domain expert:** "Right. She doesn't do anything. The Hook ran, the Substrate had new Messages, they became part of her Claude's Context. That's **Ambient awareness**."

## Flagged ambiguities

- **"Room" vs "topic"** — initially used interchangeably. Resolved: **Room** is the user-facing concept. "Topic" is reserved for the underlying gossip primitive and is *not* a domain term.
- **"Ticket" vs "Room code"** — initially synonyms. Resolved: **Ticket** is the canonical bytes; **Room code** is the short human-shareable display form.
- **"Peer" vs "user" vs "member"** — drifted. Resolved: **Peer** is the running process, **Identity** is the cryptographic actor, **Nickname** is the human label. "user" is forbidden everywhere — it collides with Claude's conversation role.
- **"Substrate" vs "Context"** — *not* synonyms. Substrate is what cc-connect stores and replicates. Context is what Claude Code sees on a turn. Conflating them breaks the architectural framing.
- **"Encrypted by default"** — ambiguous between *transport* encryption (present in v0.1 via QUIC/TLS) and *content* encryption (E2E with key derivation, deferred). Always qualify which.
