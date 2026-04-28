//! `cc-connect host` — create a Room, print its Ticket, and stay online so
//! joiners have a peer to dial.
//!
//! Implements the PROTOCOL.md §3 Ticket creation path:
//!   1. Load (or generate) the local Identity (PROTOCOL.md §2).
//!   2. Build an iroh `Endpoint` whose `EndpointId` equals our Pubkey.
//!   3. Generate a fresh 32-byte `TopicId` (PROTOCOL.md §3 Room ticket).
//!   4. Spawn `iroh-gossip` and a `Router` accepting the gossip ALPN.
//!   5. Wait until the endpoint is online.
//!   6. Pack `{topic, peers: [our_addr]}` as the ticket payload, wrap
//!      with `cc1-` + base32 + CRC32 (cc-connect-core::ticket), print.
//!   7. Idle until SIGINT/SIGTERM so joiners can dial us.

use anyhow::{anyhow, Context, Result};
use cc_connect_core::{identity::Identity, ticket::encode_room_code};
use iroh::{endpoint::RelayMode, Endpoint, RelayMap, SecretKey};
use iroh_gossip::{net::{Gossip, GOSSIP_ALPN}, proto::TopicId};
use std::path::PathBuf;

use crate::ticket_payload::TicketPayload;

pub fn run(no_relay: bool, relay: Option<&str>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    rt.block_on(run_async(no_relay, relay))
}

async fn run_async(no_relay: bool, relay: Option<&str>) -> Result<()> {
    // Step 1: load or create the local Identity.
    let identity = load_identity()?;
    let secret_key = SecretKey::from_bytes(&identity.seed_bytes());

    // Step 2: build the iroh Endpoint with our key. PROTOCOL.md §2 binds
    // the iroh `EndpointId` to our Pubkey via this same secret.
    let mut builder = Endpoint::builder(iroh::endpoint::presets::N0).secret_key(secret_key);
    if no_relay {
        builder = builder.relay_mode(RelayMode::Disabled);
    } else if let Some(url) = relay {
        let map = RelayMap::try_from_iter([url])
            .map_err(|e| anyhow!("RELAY_URL_INVALID: {url}: {e}"))?;
        builder = builder.relay_mode(RelayMode::Custom(map));
    }
    let endpoint = builder.bind().await.context("bind iroh endpoint")?;

    // Step 3: generate a fresh topic ID.
    let mut topic_bytes = [0u8; 32];
    getrandom::getrandom(&mut topic_bytes)
        .map_err(|e| anyhow::anyhow!("OS random for topic: {e}"))?;
    let topic = TopicId::from_bytes(topic_bytes);

    // Step 4: spawn gossip + a Router that accepts the gossip ALPN.
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let _router = iroh::protocol::Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .spawn();

    // Step 5: wait for the endpoint to be online so the printed ticket has
    // working bootstrap info. Skip when relay is disabled — `online()`
    // blocks on a relay-home which won't ever land in that mode (per the
    // iroh-gossip examples/chat.rs convention).
    if !no_relay {
        endpoint.online().await;
    }

    // Step 6: subscribe to our own topic so joiners have someone to bootstrap
    // off. Use `subscribe` (not `subscribe_and_join`) since we have no
    // bootstrap peers ourselves and shouldn't block waiting for one.
    // Without this, joiners' `subscribe_and_join` hangs because no member of
    // the topic ever ack's them.
    let _topic_handle = gossip.subscribe(topic, vec![]).await?;

    // Step 7: assemble and encode the ticket.
    let our_addr = endpoint.addr();
    let payload = TicketPayload {
        topic,
        peers: vec![our_addr],
    };
    let payload_bytes = payload.to_bytes()?;
    let room_code = encode_room_code(&payload_bytes);

    println!();
    println!("Room hosted. Share this code out-of-band:");
    println!();
    println!("    {}", room_code);
    println!();
    println!("Joiners run:  cc-connect chat <room-code>");
    println!();
    println!("Press Ctrl-C to close the room (joiners will be disconnected).");

    // Step 8: stay online so joiners can dial us. Drop guards on Ctrl-C.
    tokio::signal::ctrl_c()
        .await
        .context("install Ctrl-C handler")?;
    println!("\nclosing room…");
    drop(_topic_handle);
    drop(gossip);
    Ok(())
}

fn load_identity() -> Result<Identity> {
    let path = identity_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    Identity::generate_or_load(&path)
}

fn identity_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".cc-connect").join("identity.key"))
}
