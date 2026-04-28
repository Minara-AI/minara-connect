//! cc-connect library half — exposes `chat_session` (and its hard deps) so
//! the future TUI / room orchestrator can drive a chat session inproc
//! without spawning the whole `cc-connect chat` subprocess.
//!
//! The thin `cc-connect` binary at `src/main.rs` is just a clap dispatcher
//! over [`run`].

pub mod backfill;
pub mod chat;
pub mod chat_session;
pub mod doctor;
pub mod host;
pub mod host_bg;
pub mod room;
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
    /// Open the cc-connect TUI: vertical split with chat on the left and an
    /// embedded `claude` PTY on the right. Thin wrapper around the
    /// `cc-connect-tui` binary that ships next to this one.
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
    /// Sanity-check the cc-connect installation.
    Doctor,
}

#[derive(Subcommand)]
pub enum RoomCmd {
    /// Start a new Room (spawns a background host daemon) and open the TUI.
    Start {
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
    /// Join an existing Room by ticket.
    Join {
        ticket: String,
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
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
        Command::Chat { ticket, no_relay, relay } => {
            chat::run(&ticket, no_relay, relay.as_deref())
        }
        Command::Room { cmd } => match cmd {
            RoomCmd::Start { relay } => room::run_start(relay.as_deref()),
            RoomCmd::Join { ticket, relay } => room::run_join(&ticket, relay.as_deref()),
        },
        Command::HostBg { cmd } => match cmd {
            HostBgCmd::Start { relay } => host_bg::run_start(relay.as_deref()),
            HostBgCmd::Stop { topic } => host_bg::run_stop(&topic),
            HostBgCmd::List => host_bg::run_list(),
        },
        Command::HostBgDaemon { relay } => host_bg::run_daemon(relay.as_deref()),
        Command::Doctor => doctor::run(),
    }
}
