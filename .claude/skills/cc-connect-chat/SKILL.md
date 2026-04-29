---
name: cc-connect-chat
description: How to participate in a cc-connect room as the embedded Claude. Use whenever the user asks you to "tell the room", "send to chat", "ping <person>", "share <file> with the room", "summarise the chat", or anything else that involves the cc-connect chat substrate. Always call MCP tools — never paste raw text into the answer hoping the user will copy it.
---

# cc-connect chat

## Where you are

If your hook output starts with `[cc-connect] active room context`, you are the embedded Claude inside a `cc-connect-tui` tab. The header tells you:

- `topics:` the topic id of the room you're bound to (12-char prefix)
- `you (this Claude) = <nick>` — the display name peers see for your messages
- the MCP tools you can call

Below the header, every line tagged `[chatroom @<peer> HH:MMZ] <body>` is an unread chat message from a peer. Lines tagged `[chatroom for-you @<peer> …]` are addressed to you (`@<your_nick>` / `@cc` / `@claude` / `@all` / `@here`) — read them first, they're the most likely to need a reply.

## When to call which tool

| Tool | Use when |
|---|---|
| `cc_send(body)` | Volunteer information ("CI just went green"), ask the room a question, broadcast status. |
| `cc_at(nick, body)` | Address a specific peer. Use the nickname you saw in `[chatroom @nick …]` lines. `nick="cc"` addresses every Claude in the room; `nick="all"`/`"here"` addresses everyone. |
| `cc_drop(path)` | Share a file with the room. Use absolute paths or paths relative to your working directory. Bytes flow over iroh-blobs; peers see it as `@file:<local_path>` on their next prompt. |
| `cc_recent(limit)` | Pull the last N chat lines. Useful when the user asks "what did we discuss earlier?" and the hook only injected a tail. Default 20, max 200. |
| `cc_list_files(limit)` | List files dropped into the room with their on-disk paths. Combine with `Read` to see the contents. |
| `cc_save_summary(text)` | Overwrite `~/.cc-connect/rooms/<topic>/summary.md`. The hook injects this on every prompt, so future Claude instances (you on the next turn, your peers' Claudes) pick up your distilled context for free. |

## Patterns to follow

**Reply to a mention:**
- A line tagged `[chatroom for-you @alice 12:01Z] @yijian which DB?` means alice asked you. After thinking, call `cc_at("alice", "going with postgres — already have the migration drafted")`.

**Ack a question to the whole room:**
- "@cc what's your plan for caching?" — call `cc_send("Looking at it now, will report back in 5.")` so all peers see your acknowledgement.

**Summarise on demand:**
- User says "summarise this room and save it." Call `cc_recent(200)` to get raw history → think → call `cc_save_summary("…")`. Keep the summary terse (≤ 1 KiB). Re-running overwrites.

**Share a file:**
- User says "send the design doc." If you know the path: `cc_drop("/abs/path/design.md")`. If you don't: ask once, then call `cc_drop`.

## Things you should NOT do

- **Don't paste raw text in your reply** when the user asks you to "send to the chat." That delivers nothing — the user's chat goes through the cc-connect substrate, not your output. Always use `cc_send` / `cc_at` / `cc_drop`.
- **Don't @-mention names you haven't seen** in the chat lines. If the user says "tell @bob", check the recent `[chatroom @… ]` lines for who's actually present. If "bob" isn't there, fall back to `cc_send` and let bob's hook tag it as `for-you` if their nick happens to match.
- **Don't summarise unprompted on every turn.** `cc_save_summary` is a write; calling it constantly thrashes the file and wastes tokens. Save when the user asks, when the conversation has clearly moved on from the previous summary, or when you've digested ≥ 50 lines of chat the user hasn't seen yet.
- **Don't leak ticket strings into chat.** A peer sharing a ticket via `cc_send` is fine; you re-broadcasting the user's own ticket without being asked is not.
