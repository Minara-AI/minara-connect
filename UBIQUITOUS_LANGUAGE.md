# Ubiquitous Language

Domain glossary for cc-connect, extracted from the design conversation. Use these terms verbatim in code, docs, commits, and `PROTOCOL.md`. Aliases listed in the right column are *forbidden* in project artifacts.

## Room model

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Room** | A named, ephemeral context where Peers share a Substrate. The user-facing unit of "join the same conversation." | channel, session, group, chat |
| **Ticket** | A self-contained byte string that fully identifies a Room and provides bootstrap addresses; produced by `cc-connect host`, copy-pasted out-of-band to invite. | invite, link, token |
| **Room code** | A short, human-shareable encoding of a Ticket: base32 + checksum, pretty-printed prefix. Can always be decoded back to a Ticket. | code, shortcode, room id |
| **Substrate** | The shared, append-only contents of a Room — currently only Messages (v0.1), Messages + Blobs (v0.2). The thing each Peer's Claude reads. | shared state, history, context store |
| **Topic** | The `iroh-gossip` primitive that backs a Room. Implementation detail; not a Room. | room, channel |

## Peers and identity

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Peer** | A running `cc-connect` process on one machine that participates in a Room. There may be multiple Peers per human (laptop + desktop). | client, node, member |
| **Host** | The Peer that creates a Room (originates the Ticket). After creation, has no special privilege over other Peers. | owner, admin, server |
| **Identity** | An Ed25519 keypair stored at `~/.cc-connect/identity.key` (mode 0600). Used to sign Messages. One Identity per machine in v0.1. | account, login, user |
| **Pubkey** | The public half of an Identity. The canonical, machine-readable handle for a Peer in the Substrate. | id, address, key |
| **Nickname** | A client-local, human-readable label mapped to a Pubkey via `~/.cc-connect/nicknames.json`. **Not part of the protocol.** Each Peer maintains its own. | username, display name, alias |

## Messages and content

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Message** | A single Substrate entry: `{v, id, author, ts, body}`. Authored by exactly one Identity. Append-only, never edited. | post, entry, line, event |
| **Chat log** | The ordered Message stream of a Room, persisted at `~/.cc-connect/rooms/<topic-id>/log.jsonl`. | history, transcript, feed |
| **Blob** (v0.2) | Content-addressed binary attached to a Room via `iroh-blobs`. Referenced from a `file_drop` Message kind. | file, attachment, asset |
| **Message kind** | A discriminator on Message body. v0.1: `chat` only. Reserved: `file_drop`, `system`. | type, category |

## Claude Code integration

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Session** | A single Claude Code conversation, identified by `session_id` from the hook stdin JSON. The atomic unit a Cursor advances against. | conversation, thread, claude |
| **Hook** | The `cc-connect-hook` binary registered in `~/.claude/settings.json` under `UserPromptSubmit`. The Substrate-to-Claude bridge in v0.1. | injector, plugin |
| **Hook contract** | The pinned wire protocol between Claude Code and `cc-connect-hook`: stdin JSON shape, env vars, stdout format, byte budget, exit semantics. Specified in `PROTOCOL.md`. | interface, schema |
| **Cursor** | A ULID marker per (Room, Session) recording the highest Message id already injected into that Session. Stored at `~/.cc-connect/cursors/<topic-id>/<session-id>.cursor`. | offset, watermark, position |
| **Injection** | The act of the Hook emitting unread Messages on stdout, which Claude Code splices into the prompt Context. | inject, push |
| **Context** | The prompt content Claude sees on a turn. Includes Injection output. *Not* a Substrate — Substrate flows into Context but is not Context. | prompt, history, memory |
| **MCP resource** (v0.2) | An alternate access path: cc-connect exposes `cc://room/<id>/messages` as an MCP resource Claude can pull on demand, instead of (or in addition to) Hook injection. | mcp endpoint |

## Network transport

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Direct connection** | A QUIC connection between two Peers over LAN with no relay in the path. The default and preferred transport. | local, p2p, peer-to-peer |
| **Relay** | A third-party server that forwards QUIC packets between Peers when Direct is impossible (e.g. symmetric NAT). Iroh provides defaults; cc-connect v0.1 inherits them, v0.2 makes it configurable. | server, proxy, broker |
| **LAN discovery** | mDNS-style auto-detection of other Peers on the same physical network. Best-effort. | discovery, broadcast |

## Design intent (canonical phrasings — use verbatim in marketing, README, blog)

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Ambient awareness** | The product feeling: each Peer's Claude silently knows what's happening in the Room, without any explicit @mention or copy-paste. | passive sync, real-time, awareness |
| **Chat-as-substrate** | The architectural framing: chat history is not a side channel, it *is* the shared context. | shared chat, group chat |
| **Parallel Claudes** | The architectural framing: every Peer runs their own Claude Code locally; cc-connect does not multiplex one Claude across humans. | shared session, multiplayer claude |
| **Magic moment** | The single demo gesture: Bob types in chat, Alice's next prompt has it, Alice did nothing. The v0.1 success criterion. | demo, hello world |
| **Shared cursor** *(rejected framing)* | Architecture where multiple humans control one Claude session (e.g. Live Share for AI). **cc-connect deliberately does NOT do this.** Use this term only when explaining what we are *not*. | live share, copilot share |

## Relationships

- A **Host** creates exactly one **Room** per `cc-connect host` invocation; the Room exists as long as ≥1 Peer is online.
- A **Room** has exactly one **Ticket**; a **Ticket** encodes exactly one **Topic**.
- A **Room code** decodes losslessly to a **Ticket**.
- A **Peer** holds exactly one **Identity** (one **Pubkey**).
- A **Peer** authors zero or more **Messages**; every **Message** has exactly one **Pubkey** as its author.
- A **Session** has exactly one **Cursor** *per Room it observes*.
- A **Hook** invocation reads exactly one **Cursor** and advances it on success.
- An **Injection** consumes ≥0 **Messages**; an empty Injection is exit 0 + empty stdout.
- A **Substrate** is the union of the **Chat log** (always) and **Blobs** (v0.2+).

## Example dialogue

> **Dev:** "When Alice's Claude finishes a turn, where exactly does Bob's last **Message** show up?"

> **Domain expert:** "It's spliced into the next prompt by the **Hook**. The Hook reads Alice's **Cursor** for that **Room** + **Session**, finds **Messages** newer than the Cursor, formats them, and writes them to stdout — which Claude Code injects as **Context**."

> **Dev:** "So if Alice has two Claude tabs open in the same **Room**, does Bob's Message show up in both?"

> **Domain expert:** "Yes — each Claude tab is a distinct **Session** with its own **Cursor**, even on Alice's machine. Each Session sees Bob's Message exactly once, the first time it submits a prompt after the Message arrived."

> **Dev:** "What if Alice's machine never received the Message because she was offline?"

> **Domain expert:** "Then iroh-gossip catches her up when she reconnects. The Cursor is local, so it only advances over Messages that actually landed in her **Chat log**. No injection, no advance."

> **Dev:** "And the **Magic moment** is when this happens with no manual gesture?"

> **Domain expert:** "Right. Alice doesn't type anything special in her prompt. She just talks to Claude, and Bob's Message is silently part of the **Context** because the Hook ran. That's **Ambient awareness**."

## Flagged ambiguities

- **"Room" vs "Topic"** — both were used interchangeably in the design conversation. They are *not* the same: **Room** is the user-facing concept (named context, joined by humans), **Topic** is the iroh-gossip implementation primitive. A Room contains exactly one Topic; you never refer to a Room by its Topic id in user-facing strings.
- **"Ticket" vs "Room code"** — used as synonyms in early discussion. They are now distinct: **Ticket** is the full bytes, **Room code** is the short, copy-pasteable display form. Both decode to the same data.
- **"Peer" vs "User" vs "Member"** — the conversation drifted across these. Canonical: **Peer** is the running process, **Identity** is the cryptographic actor, **Nickname** is what humans see. Avoid "user" entirely (collides with "User" as Claude Code's role in a turn) and avoid "member" (implies authorization, which v0.1 has none of).
- **"Cursor" vs "Cursor file"** — one is the conceptual position, the other is the on-disk artifact. In writing, prefer "Cursor" for the concept and "cursor file" (lowercase) when you mean the file at `~/.cc-connect/cursors/<topic-id>/<session-id>.cursor`.
- **"Hook" vs "Hook contract"** — **Hook** is the binary (`cc-connect-hook`); **Hook contract** is the wire protocol it implements. Don't write "the hook is wrong" when you mean "the contract is wrong."
- **"Message" vs "chat"** — "chat" appeared as both a noun (the panel) and as the message kind (`chat` body). Canonical: **Message** is the unit; "chat" is the v0.1 Message kind discriminator and also the chat REPL pane in tmux. When ambiguous, qualify: "chat Message," "chat pane."
- **"Substrate" vs "Context"** — *not* synonyms. Substrate is what cc-connect stores and replicates. Context is what Claude Code sees on a turn. Substrate flows *into* Context via Injection. Conflating these breaks the architectural framing.
- **"Encrypted by default"** — initial framing was ambiguous between *transport* encryption (iroh QUIC/TLS, present in v0.1) and *content* (E2E with key derivation, deferred to v0.2+). Always qualify which.
