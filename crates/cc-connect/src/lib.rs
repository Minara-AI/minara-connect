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
    /// Sanity-check the cc-connect installation.
    Doctor,
}

pub fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Command::Host { no_relay, relay } => host::run(no_relay, relay.as_deref()),
        Command::Chat { ticket, no_relay, relay } => {
            chat::run(&ticket, no_relay, relay.as_deref())
        }
        Command::Doctor => doctor::run(),
    }
}
