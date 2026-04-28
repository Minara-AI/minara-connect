//! `cc-connect chat <ticket>` — REPL adapter over [`crate::chat_session`].
//!
//! All the iroh-gossip + iroh-blobs + log-append + active-rooms wiring
//! lives in `chat_session`. This module is just the stdin/stdout/Ctrl-C
//! glue that turns a chat session into a terminal-friendly command.
//!
//! UX is byte-identical to the previous in-line implementation; the
//! existing smoke test (`scripts/smoke-test.sh`) is the authoritative
//! contract.

use anyhow::{Context, Result};

use crate::chat_session::{self, ChatSessionConfig, DisplayLine};

pub fn run(ticket_str: &str, no_relay: bool, relay: Option<&str>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    rt.block_on(run_async(ticket_str, no_relay, relay))
}

async fn run_async(ticket_str: &str, no_relay: bool, relay: Option<&str>) -> Result<()> {
    let cfg = ChatSessionConfig {
        ticket: ticket_str.to_string(),
        no_relay,
        relay: relay.map(|s| s.to_string()),
    };
    let mut handle = chat_session::spawn(cfg).await?;

    // Render display lines. Header banner + REPL state cues — kept format-
    // identical to the previous chat REPL (smoke-test asserts this).
    let stdout_task = tokio::spawn(async move {
        let mut joined_header_done = false;
        while let Some(line) = handle.display_rx.recv().await {
            match line {
                DisplayLine::System(s) => {
                    // The first two System lines are the join banner; print a
                    // leading blank line before the first one for parity with
                    // the old layout.
                    if !joined_header_done {
                        println!();
                    }
                    println!("{s}");
                    joined_header_done = true;
                }
                DisplayLine::Marker(s) => println!("{s}"),
                DisplayLine::Incoming { nick_short, body } => {
                    println!("[{nick_short}] {body}");
                }
                DisplayLine::Echo(s) => println!("{s}"),
                DisplayLine::Warn(s) => eprintln!("{s}"),
            }
        }
    });

    // After both header lines + (optional) marker have flushed, print the
    // REPL prompt + a trailing blank line, matching the previous UX. Race-
    // wise this can show up before/after the marker; the previous code had
    // the same property (the listener task printed concurrently with the
    // banner block). Acceptable for v0.2.
    let input_tx = handle.input_tx.clone();
    let stdin_task = tokio::spawn(async move {
        // Tiny delay so the banner lands first in the common case.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        println!("Type to send. Ctrl-C / EOF to leave.");
        println!();
        if let Err(e) = forward_stdin(input_tx).await {
            eprintln!("[chat] stdin error: {e:#}");
        }
    });

    // Wait for either: stdin EOF (input_tx dropped → session shuts down via
    // Ok branch), Ctrl-C (we abort), or session crash.
    let result = tokio::select! {
        r = &mut handle.join => r.context("chat session task panicked")?,
        _ = tokio::signal::ctrl_c() => {
            println!("\n[chat] Ctrl-C — leaving room");
            // Force the chat session to unwind: aborting handle.join drops
            // its run_session future, which drops display_tx and the
            // listener task. stdout_task then sees None on display_rx and
            // exits. Without this, stdin_task's detached read_line still
            // holds an input_tx clone, so the chat session would never
            // realise the user wanted to leave.
            handle.join.abort();
            Ok(())
        }
    };

    stdin_task.abort();
    let _ = stdout_task.await;
    result
}

async fn forward_stdin(input_tx: tokio::sync::mpsc::Sender<String>) -> Result<()> {
    use tokio::io::AsyncBufReadExt;
    let mut stdin_reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    loop {
        line.clear();
        let n = stdin_reader
            .read_line(&mut line)
            .await
            .context("read stdin")?;
        if n == 0 {
            break Ok(());
        }
        if input_tx.send(line.clone()).await.is_err() {
            break Ok(());
        }
    }
}
