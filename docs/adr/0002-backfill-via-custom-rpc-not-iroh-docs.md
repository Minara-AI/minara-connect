# Backfill uses a custom point-to-point RPC, not iroh-docs

When a Peer joins a Room, we transfer up to the last 50 Messages from any already-connected Peer via a small request-response protocol over `iroh-net` direct connections. The chat substrate itself remains an `iroh-gossip` topic — gossip handles real-time fan-out, the custom RPC handles the catch-up.

We considered using `iroh-docs` (multi-writer keyspace with range-based set reconciliation), which would solve Backfill automatically. We rejected it for v0.1 because it adds a heavier dependency and a stronger semantic surface than we need (full history replication, set-CRDT reasoning), and because keeping gossip and Backfill as separate concerns makes the protocol legible — the RPC is ~100 lines and easy to reason about per-operation.

The upgrade path to `iroh-docs` is open. If real-world use shows v0.1's bounded Backfill is insufficient, swapping the local Chat log storage to `iroh-docs` is an additive change to the implementation; the protocol-level Message format does not change.

## Wire-level details (pinned by /plan-eng-review)

- **ALPN:** `cc-connect/v1/backfill`. Distinct from the gossip ALPN (`iroh-gossip` provides its own); both protocols ride the same iroh endpoint via ALPN-based routing.
- **Wire format:** length-prefixed JSON (4-byte big-endian length + UTF-8 JSON body). One request, one response, then close the stream.
- **Request:** `{"since": <ulid_string_or_null>, "limit": 50}`. `since` is exclusive; `null` means "give me your latest 50."
- **Response:** `{"messages": [<Message>, ...]}` ordered by ULID ascending. `messages` may be empty if responder has no log for the Room.
- **Timeout:** 5 seconds from open-stream to receiving the full response. On timeout, joiner tries the next online peer; if none, joiner proceeds without backfill and surfaces a `[chatroom] (joined late, no history available)` marker.
