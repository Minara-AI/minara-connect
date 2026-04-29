//! One [`RoomTab`] per joined room. Each tab owns its own chat session and
//! its own embedded `claude` PTY child. The TUI multiplexes input/output
//! across tabs via fan-in mpsc channels keyed by `TabId`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use cc_connect::chat_session::{self, ChatHandle, ChatSessionConfig};
use cc_connect::ticket_payload::TicketPayload;
use cc_connect_core::ticket::decode_room_code;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::ChatLine;

/// Stable identifier for a tab; never reused even when tabs close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u64);

/// Bytes from one tab's claude PTY, to be fed into that tab's vt100 parser.
pub type PtyChunk = (TabId, Vec<u8>);

/// One tab's display line, to be appended to that tab's scrollback.
pub type DisplayEvent = (TabId, cc_connect::chat_session::DisplayLine);

pub struct RoomTab {
    pub id: TabId,
    pub topic_hex: String,
    pub ticket: String,

    /// True if WE started the host-bg daemon for this room. Used by the
    /// close-tab confirm prompt.
    pub hosting: bool,

    /// Chat-session handle. `chat_handle.input_tx.send(line)` broadcasts.
    /// We don't poll its display_rx ourselves — a forwarder task does that
    /// and pushes into `App.display_rx` with this tab's `TabId`.
    pub chat_handle: ChatHandle,

    /// PTY plumbing. `pty_master` is kept so we can `.resize(...)` on
    /// terminal resize; `pty_writer` is used to forward keystrokes.
    pub pty_master: Box<dyn MasterPty + Send>,
    pub pty_writer: Box<dyn std::io::Write + Send>,
    pub pty_child: Box<dyn portable_pty::Child + Send + Sync>,

    /// vt100 screen state, fed by PTY bytes.
    pub vt_parser: vt100::Parser,

    /// Per-tab chat scrollback (rendered in left pane when this tab is active).
    pub chat_lines: VecDeque<ChatLine>,
    /// Chat scrollback offset, in lines back from the live bottom.
    /// 0 = follow bottom; new messages auto-pin to the latest. >0 holds
    /// the user N lines back so peer chat doesn't push their reading
    /// position out from under them.
    pub chat_scroll: u16,
    /// User-controlled claude pane scrollback offset (rows up from live).
    /// 0 = follow live output. Applied via `vt100::Screen::set_scrollback`
    /// just before each draw of the active tab.
    pub claude_scroll: u16,
    /// Per-tab textbox.
    pub input_buf: String,

    /// Distinct peer nicks we've seen on this tab, most-recent-first.
    /// Drives the @-mention completion popup.
    pub recent_nicks: VecDeque<String>,
    /// Currently-selected entry index in the filtered mention list.
    /// Reset to 0 every time the input buffer mutates.
    pub mention_idx: usize,
    /// User pressed Esc on the popup. Suppresses display until the
    /// in-progress @-token changes (typing or backspace).
    pub mention_dismissed: bool,

    /// Background tasks we own (forwarders + pty reader). Aborted on drop.
    forwarder_tasks: Vec<JoinHandle<()>>,
    pty_reader_task: Option<JoinHandle<()>>,
}

const CHAT_SCROLLBACK_CAP: usize = 1024;
const RECENT_NICKS_CAP: usize = 32;

impl RoomTab {
    pub fn push_chat(&mut self, line: ChatLine) {
        if self.chat_lines.len() >= CHAT_SCROLLBACK_CAP {
            self.chat_lines.pop_front();
        }
        self.chat_lines.push_back(line);
    }

    /// Record a nick we just saw from a peer. Most-recent-first, deduped,
    /// capped at [`RECENT_NICKS_CAP`]. Drives the @-mention popup.
    pub fn record_nick(&mut self, nick: &str) {
        let trimmed = nick.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(pos) = self.recent_nicks.iter().position(|n| n == trimmed) {
            self.recent_nicks.remove(pos);
        }
        self.recent_nicks.push_front(trimmed.to_string());
        while self.recent_nicks.len() > RECENT_NICKS_CAP {
            self.recent_nicks.pop_back();
        }
    }

    pub fn topic_short(&self) -> String {
        self.topic_hex
            .chars()
            .take(12.min(self.topic_hex.len()))
            .collect()
    }
}

impl Drop for RoomTab {
    fn drop(&mut self) {
        for h in self.forwarder_tasks.drain(..) {
            h.abort();
        }
        if let Some(h) = self.pty_reader_task.take() {
            h.abort();
        }
        // chat_handle drops naturally → input_tx closes → run_session
        // unwinds. Plus we drop the chat_handle's join handle here too.
        self.chat_handle.join.abort();
        let _ = self.pty_child.kill();
    }
}

pub struct SpawnTabArgs {
    pub ticket: String,
    pub no_relay: bool,
    pub relay: Option<String>,
    pub claude_argv: Vec<String>,
    pub claude_cwd: Option<PathBuf>,
    pub initial_pty_size: PtySize,
    pub hosting: bool,
}

pub struct TabIo {
    /// Each tab's display-line forwarder pushes onto this shared mpsc.
    pub display_tx: mpsc::Sender<DisplayEvent>,
    /// PTY-byte forwarder pushes onto this shared mpsc.
    pub pty_tx: mpsc::Sender<PtyChunk>,
}

/// Boot a tab: decode ticket, start chat_session, spawn claude in a PTY,
/// wire up the two forwarder tasks (chat_session display_rx → fan-in mpsc,
/// PTY bytes → fan-in mpsc), return the assembled `RoomTab`.
pub async fn spawn_tab(id: TabId, args: SpawnTabArgs, io: &TabIo) -> Result<RoomTab> {
    // Topic from ticket — re-decode locally so the caller doesn't have to.
    let payload_bytes =
        decode_room_code(&args.ticket).with_context(|| format!("decode ticket {:.20}…", args.ticket))?;
    let payload = TicketPayload::from_bytes(&payload_bytes)?;
    let topic_hex = topic_to_hex(payload.topic.as_bytes());

    // Boot the chat session.
    let cfg = ChatSessionConfig {
        ticket: args.ticket.clone(),
        no_relay: args.no_relay,
        relay: args.relay.clone(),
    };
    let mut chat_handle = chat_session::spawn(cfg)
        .await
        .context("spawn chat_session")?;

    // Forward chat_session DisplayLines onto the fan-in display_tx with this tab's id.
    // We need to take display_rx out of the handle since it's an owned Receiver.
    // Replace it in the handle with a dummy that immediately ends — the handle's
    // input_tx + join are what we keep using.
    let (dummy_tx, dummy_rx) = mpsc::channel::<cc_connect::chat_session::DisplayLine>(1);
    drop(dummy_tx);
    let mut display_rx = std::mem::replace(&mut chat_handle.display_rx, dummy_rx);
    let display_tx = io.display_tx.clone();
    let display_forwarder = tokio::spawn(async move {
        while let Some(line) = display_rx.recv().await {
            if display_tx.send((id, line)).await.is_err() {
                break;
            }
        }
    });

    // Spawn claude in a PTY, sized to the right pane area.
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(args.initial_pty_size)
        .map_err(|e| anyhow!("openpty: {e}"))?;

    let mut cmd = CommandBuilder::new(&args.claude_argv[0]);
    for arg in args.claude_argv.iter().skip(1) {
        cmd.arg(arg);
    }
    cmd.env("CC_CONNECT_ROOM", &topic_hex);
    if let Some(cwd) = &args.claude_cwd {
        cmd.cwd(cwd);
    }
    let pty_child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow!("spawn claude in PTY: {e}"))?;

    let pty_master = pair.master;
    let pty_writer = pty_master
        .take_writer()
        .map_err(|e| anyhow!("pty take_writer: {e}"))?;
    let mut pty_reader_box = pty_master
        .try_clone_reader()
        .map_err(|e| anyhow!("pty try_clone_reader: {e}"))?;
    let pty_tx = io.pty_tx.clone();
    let pty_reader_task = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            match pty_reader_box.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if pty_tx.blocking_send((id, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(RoomTab {
        id,
        topic_hex,
        ticket: args.ticket,
        hosting: args.hosting,
        chat_handle,
        pty_master,
        pty_writer,
        pty_child,
        // Scrollback capacity 5000 rows: enough to look back through a
        // long claude session, small enough to not balloon memory across
        // many tabs. Without this (the previous `0`) `set_scrollback`
        // had nothing to show, which is why the pane refused to scroll.
        vt_parser: vt100::Parser::new(
            args.initial_pty_size.rows,
            args.initial_pty_size.cols,
            5000,
        ),
        chat_lines: VecDeque::new(),
        chat_scroll: 0,
        claude_scroll: 0,
        input_buf: String::new(),
        recent_nicks: VecDeque::new(),
        mention_idx: 0,
        mention_dismissed: false,
        forwarder_tasks: vec![display_forwarder],
        pty_reader_task: Some(pty_reader_task),
    })
}

fn topic_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// HashMap-of-tabs + ordering Vec, used as App state.
pub struct TabSet {
    pub tabs: HashMap<TabId, RoomTab>,
    pub order: Vec<TabId>,
    pub active: Option<TabId>,
    pub next_id: u64,
}

impl Default for TabSet {
    fn default() -> Self {
        Self::new()
    }
}

impl TabSet {
    pub fn new() -> Self {
        Self {
            tabs: HashMap::new(),
            order: Vec::new(),
            active: None,
            next_id: 0,
        }
    }

    pub fn alloc_id(&mut self) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn add(&mut self, tab: RoomTab) {
        let id = tab.id;
        self.tabs.insert(id, tab);
        self.order.push(id);
        if self.active.is_none() {
            self.active = Some(id);
        } else {
            self.active = Some(id); // focus newly opened tab
        }
    }

    pub fn remove(&mut self, id: TabId) -> Option<RoomTab> {
        self.order.retain(|i| *i != id);
        if self.active == Some(id) {
            self.active = self.order.last().copied();
        }
        self.tabs.remove(&id)
    }

    pub fn active_tab(&self) -> Option<&RoomTab> {
        self.active.and_then(|id| self.tabs.get(&id))
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut RoomTab> {
        let id = self.active?;
        self.tabs.get_mut(&id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut RoomTab> {
        self.tabs.get_mut(&id)
    }

    pub fn switch_to_index(&mut self, idx: usize) {
        if let Some(&id) = self.order.get(idx) {
            self.active = Some(id);
        }
    }

    pub fn cycle(&mut self, delta: i32) {
        if self.order.is_empty() {
            return;
        }
        let cur_idx = self
            .active
            .and_then(|id| self.order.iter().position(|i| *i == id))
            .unwrap_or(0) as i32;
        let n = self.order.len() as i32;
        let new = (cur_idx + delta).rem_euclid(n) as usize;
        self.active = Some(self.order[new]);
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }
}
