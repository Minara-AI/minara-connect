//! cc-connect — `host`, `chat`, `doctor`. Thin clap dispatcher over the
//! library half (`crates/cc-connect/src/lib.rs`).

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    cc_connect::run(cc_connect::Cli::parse())
}
