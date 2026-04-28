//! cc-connect — `host`, `chat`, `doctor`.
//!
//! Subcommands track PROTOCOL.md §3 (Ticket), §6.1–§6.2 (transport),
//! §7.1 (Hook install path), and §8 (active-rooms protocol).

use anyhow::Result;
use clap::{Parser, Subcommand};

mod chat;
mod doctor;
mod host;
mod ticket_payload;

#[derive(Parser)]
#[command(name = "cc-connect", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new Room and print its Ticket. Exits after printing.
    Host,
    /// Join a Room and run the chat REPL. Long-running.
    Chat {
        /// Room code (`cc1-…`) shared out-of-band by the Host.
        ticket: String,
    },
    /// Sanity-check the cc-connect installation.
    Doctor,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Host => host::run(),
        Command::Chat { ticket } => chat::run(&ticket),
        Command::Doctor => doctor::run(),
    }
}
