# Hook discovers active Rooms via a PID-based directory, not env vars

The Hook needs to know which Rooms it should inject Messages from. We use a directory of marker files at **`/tmp/cc-connect-$UID/active-rooms/<topic-id>.active`** (not under `~`), each containing the PID of a running `cc-connect chat` process. The Hook iterates the directory, calls `kill(pid, 0)` to detect liveness, deletes stale entries, and injects from rooms whose PID is alive.

Per-machine `/tmp` placement is deliberate: PID is a per-machine identifier and PID files must not sync across machines. A user with a cloud-synced home directory (Dropbox, iCloud, NFS) would otherwise see another machine's PIDs and get arbitrary `kill(pid, 0)` answers. Linux and macOS both expose `/tmp` as per-machine ephemeral storage; reboots clearing the directory is correct semantics for runtime active-rooms state. Persistent state (Identity key, log.jsonl, cursor files) remains under `~/.cc-connect/`.

We rejected env-var-based wiring (e.g. `CC_CONNECT_ROOM`) because the Hook process is spawned by Claude Code, not by the chat process — env vars do not propagate across that boundary. We rejected a single `~/.cc-connect/active-room` file because v0.2 will likely want multi-Room subscription per machine and we did not want to refactor the IPC then. We rejected a Unix domain socket because it adds a daemon to a CLI tool that has no other reason to run a server.

In v0.1 the hook injects unread Messages from every active Room into every Claude Session. v0.2 may add per-Session Room subscription (so Alice's Claude working on Room A doesn't see Room B's chatter); the data layout already supports this — a `~/.cc-connect/sessions/<session-id>/subscribed-rooms` set is an additive change to the protocol.

## Cursor concurrency (pinned by /plan-eng-review)

Claude Code may fire `UserPromptSubmit` twice within milliseconds in the same Session (retry, tool-call follow-up). Without protection, two hook processes read the same Cursor and inject the same Messages twice — Claude sees duplicated context.

v0.1 uses an `fcntl` advisory exclusive lock (`LOCK_EX`) on the per-(Room, Session) cursor file: hook acquires the lock, reads Cursor, formats stdout, advances Cursor, releases lock. POSIX-portable on macOS and Linux (the v0.1 supported platforms). Approximately 10 lines of Rust on top of the cursor I/O. Lock contention is rare in practice but eliminates the duplicate-injection failure mode entirely.
