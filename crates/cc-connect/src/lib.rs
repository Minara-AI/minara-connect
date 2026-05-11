//! cc-connect library half — exposes `chat_session` (and its hard deps) so
//! the future TUI / room orchestrator can drive a chat session inproc
//! without spawning the whole `cc-connect chat` subprocess.
//!
//! The thin `cc-connect` binary at `src/main.rs` is just a clap dispatcher
//! over [`run`].

pub mod accept;
pub mod backfill;
pub mod chat;
pub mod chat_daemon;
pub mod chat_session;
pub mod doctor;
pub mod gossip_debug;
pub mod host;
pub mod host_bg;
pub mod launcher_paths;
pub mod lifecycle;
pub mod room;
pub mod setup;
pub mod ticket_payload;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cc-connect", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a new Room and print its Ticket. Exits after printing.
    Host {
        /// Disable n0's hosted relay servers; LAN-direct only.
        ///
        /// Useful for offline / pure-LAN demos where both peers are on the
        /// same network. Joiners MUST also use `--no-relay` and must be
        /// reachable directly (no NAT between them).
        #[arg(long, conflicts_with = "relay")]
        no_relay: bool,
        /// Use this self-hosted iroh-relay instead of n0's hosted relays.
        ///
        /// Pass an HTTPS URL like `https://relay.yourdomain.com`. Joiners
        /// don't need to specify `--relay`: the host's relay URL is baked
        /// into the printed ticket, and joiners pick it up automatically.
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
    /// Join a Room and run the chat REPL. Long-running.
    Chat {
        /// Room code (`cc1-…`) shared out-of-band by the Host.
        ticket: String,
        /// Disable n0's hosted relay servers; LAN-direct only.
        #[arg(long, conflicts_with = "relay")]
        no_relay: bool,
        /// Use this self-hosted iroh-relay (HTTPS URL). If unset, the
        /// joiner uses n0's defaults; the ticket's relay info still drives
        /// connection establishment.
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
    /// Open a Room. Detects an installed terminal multiplexer (zellij
    /// preferred, tmux fallback) and spawns a 60/40 layout with `claude`
    /// on the left and `cc-chat-ui` on the right; both panes inherit
    /// `CC_CONNECT_ROOM` so the hook fires + chat-ui finds chat.sock.
    /// If neither multiplexer is installed, falls back to the embedded
    /// `cc-connect-tui` single-window mode.
    Room {
        #[command(subcommand)]
        cmd: RoomCmd,
    },
    /// Run, manage, and inspect persistent host daemons.
    ///
    /// A host daemon owns a Room's topic + identity in the background,
    /// surviving the TUI / chat process that started it. The TUI uses this
    /// to keep a Room joinable after the user closes the window.
    HostBg {
        #[command(subcommand)]
        cmd: HostBgCmd,
    },
    /// Internal: daemon entry point invoked by `host-bg start`. Don't run
    /// directly — the parent `host-bg start` does the spawn-with-setsid
    /// dance and reads the READY line before exiting.
    #[command(hide = true)]
    HostBgDaemon {
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
    /// Manage persistent chat-session daemons.
    ///
    /// A chat daemon owns the gossip mesh + chat.sock IPC for one Room in
    /// the background, surviving the TUI / chat-ui panel that started it.
    /// The launcher uses this to decouple the chat substrate from any
    /// single window: closing zellij/tmux leaves the daemon running so
    /// peers continue to see your messages.
    ChatDaemon {
        #[command(subcommand)]
        cmd: ChatDaemonCmd,
    },
    /// Internal: chat-daemon entry point invoked by `chat-daemon start`.
    /// Don't run directly — the parent does the setsid spawn + READY
    /// handshake.
    #[command(hide = true, name = "chat-daemon-daemon")]
    ChatDaemonDaemon {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        no_relay: bool,
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
    /// Sanity-check the cc-connect installation.
    Doctor,
    /// Stop every running cc-connect background process (chat-daemons +
    /// host-bg). Use after `room start` panics, when `chat-daemon list`
    /// shows stuck daemons, or before re-installing a freshly-built
    /// binary so the new MCP server takes effect.
    Clear {
        /// Also remove `~/.cc-connect/rooms/` (every room's log + files
        /// + summary). Identity and nicknames are preserved.
        #[arg(long)]
        purge: bool,
    },
    /// Reverse `install.sh`: clear, strip the cc-connect-hook entry from
    /// `~/.claude/settings.json`, strip the cc-connect MCP server from
    /// `~/.claude.json`, and remove the `~/.local/bin` symlinks. Backup
    /// files are written next to each mutated JSON.
    Uninstall {
        /// Also remove `~/.cc-connect/` entirely (identity + nicknames +
        /// rooms — full factory reset).
        #[arg(long)]
        purge: bool,
    },
    /// Pull latest source + rebuild + reinstall in one shot. Equivalent
    /// to `git fetch && git pull && cc-connect uninstall && ./install.sh`
    /// from the install clone, but never strips identity/nicknames so
    /// you keep your stable Pubkey across upgrades. The `--yes` flag
    /// skips the y/N confirmation.
    Upgrade {
        /// Skip the y/N confirmation after showing the incoming commits.
        #[arg(long)]
        yes: bool,
    },
    /// Approve a Claude's pending `cc_join_room` request. The MCP-first
    /// trust boundary (PROTOCOL.md §7.3 step 0) requires explicit human
    /// consent before a Claude is bound to a room it asked to join. Get
    /// the token from the Claude's `cc_join_room` response, from
    /// `cc-connect pending-list`, or from the side-channel viewer
    /// (`cc-connect watch`, the VSCode chat panel).
    Accept {
        /// Pending-join token, from `cc_join_room`'s response or
        /// `cc-connect pending-list`.
        token: String,
    },
    /// List every pending `cc_join_room` request awaiting human
    /// consent. Use to audit what Claude has asked for, or to fish out
    /// a token after dismissing the original `cc_join_room` reply.
    PendingList,
}

#[derive(Subcommand)]
pub enum ChatDaemonCmd {
    /// Start a new chat-session daemon for `<ticket>`. Idempotent: if a
    /// daemon already owns the same topic, prints `ALREADY <topic> <pid>`
    /// and exits 0 without spawning.
    Start {
        /// Room code (`cc1-…`).
        ticket: String,
        /// Disable n0's hosted relay servers; LAN-direct only.
        #[arg(long, conflicts_with = "relay")]
        no_relay: bool,
        /// Use this self-hosted iroh-relay (HTTPS URL).
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
    /// SIGTERM a running daemon by topic hex (prefix-match accepted).
    Stop {
        /// Topic hex (full 64 chars or any unique prefix).
        topic: String,
    },
    /// List running chat daemons (one line per daemon).
    List,
}

#[derive(Subcommand)]
pub enum RoomCmd {
    /// Start a new Room (spawns a background host daemon) and open the TUI.
    Start {
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        /// Override / set the saved display name. Persists.
        #[arg(long, value_name = "NAME")]
        nick: Option<String>,
        /// Persist the per-machine `owner_only_mentions` preference.
        /// Default OFF: any peer's @-mention can wake your Claude. ON:
        /// only your own typed @-mentions wake your Claude (peer
        /// @-mentions still render but never auto-reply). Persists to
        /// `~/.cc-connect/config.json`. Use `--owner-only-mentions=false`
        /// to clear an earlier ON.
        #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
        owner_only_mentions: Option<bool>,
        /// Args forwarded to `claude`. Use `--` to separate.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        claude_args: Vec<String>,
    },
    /// Join an existing Room by ticket.
    Join {
        ticket: String,
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        /// Override / set the saved display name. Persists.
        #[arg(long, value_name = "NAME")]
        nick: Option<String>,
        /// Same per-machine preference as `room start --owner-only-mentions`.
        #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
        owner_only_mentions: Option<bool>,
        /// Args forwarded to `claude`. Use `--` to separate.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        claude_args: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum HostBgCmd {
    /// Start a new background-host daemon. Prints the Ticket on stdout
    /// then exits, leaving the daemon running detached.
    Start {
        /// Use this self-hosted iroh-relay (HTTPS URL).
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
    /// SIGTERM a running daemon by topic hex (prefix-match accepted).
    Stop {
        /// Topic hex (full 64 chars or any unique prefix).
        topic: String,
    },
    /// List all running daemons (one line per daemon).
    List,
}

pub fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Command::Host { no_relay, relay } => host::run(no_relay, relay.as_deref()),
        Command::Chat {
            ticket,
            no_relay,
            relay,
        } => chat::run(&ticket, no_relay, relay.as_deref()),
        Command::Room { cmd } => match cmd {
            RoomCmd::Start {
                relay,
                nick,
                owner_only_mentions,
                claude_args,
            } => {
                if let Some(flag) = owner_only_mentions {
                    setup::set_owner_only_mentions(flag)?;
                }
                room::run_start(relay.as_deref(), nick.as_deref(), &claude_args)
            }
            RoomCmd::Join {
                ticket,
                relay,
                nick,
                owner_only_mentions,
                claude_args,
            } => {
                if let Some(flag) = owner_only_mentions {
                    setup::set_owner_only_mentions(flag)?;
                }
                room::run_join(&ticket, relay.as_deref(), nick.as_deref(), &claude_args)
            }
        },
        Command::HostBg { cmd } => match cmd {
            HostBgCmd::Start { relay } => host_bg::run_start(relay.as_deref()),
            HostBgCmd::Stop { topic } => host_bg::run_stop(&topic),
            HostBgCmd::List => host_bg::run_list(),
        },
        Command::HostBgDaemon { relay } => host_bg::run_daemon(relay.as_deref()),
        Command::ChatDaemon { cmd } => match cmd {
            ChatDaemonCmd::Start {
                ticket,
                no_relay,
                relay,
            } => chat_daemon::run_start(&ticket, no_relay, relay.as_deref()),
            ChatDaemonCmd::Stop { topic } => chat_daemon::run_stop(&topic),
            ChatDaemonCmd::List => chat_daemon::run_list(),
        },
        Command::ChatDaemonDaemon {
            ticket,
            no_relay,
            relay,
        } => chat_daemon::run_daemon(&ticket, no_relay, relay.as_deref()),
        Command::Doctor => doctor::run(),
        Command::Clear { purge } => lifecycle::run_clear(purge),
        Command::Uninstall { purge } => lifecycle::run_uninstall(purge),
        Command::Upgrade { yes } => lifecycle::run_upgrade(yes),
        Command::Accept { token } => accept::run_accept(&token),
        Command::PendingList => accept::run_pending_list(),
    }
}
