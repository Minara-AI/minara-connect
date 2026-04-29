//! Main TUI event loop: multiplexes input + output across tabs.
//!
//! Architecture:
//!   - One [`crate::tabs::RoomTab`] per joined room. Each tab spawns a
//!     forwarder task that fans-in its `chat_session` display lines and
//!     PTY bytes onto two shared mpsc channels keyed by `TabId`.
//!   - The event loop `tokio::select!`s over: crossterm events, the
//!     fan-in display channel, the fan-in PTY channel, a periodic tick.
//!   - Render path picks the active tab and draws its panes; idle tabs
//!     keep accumulating state in the background.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use cc_connect::chat_session::DisplayLine;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event as CtEvent, EventStream, KeyCode,
        KeyEvent, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use futures_lite::StreamExt;
use portable_pty::PtySize;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame, Terminal,
};
use std::io::Write as _;
use tokio::sync::mpsc;

use crate::app::{App, ChatLine, ChatLineKind, Focus, Overlay};
use crate::tabs::{spawn_tab, DisplayEvent, PtyChunk, SpawnTabArgs, TabId, TabIo};
use crate::{chat_pane, claude_pane, theme};

/// Caller-supplied configuration for the initial tab.
pub struct RunOpts {
    pub ticket: String,
    pub topic_hex: String,
    pub no_relay: bool,
    pub relay: Option<String>,
    pub claude_argv: Vec<String>,
    pub claude_cwd: Option<PathBuf>,
    /// True if the caller spawned a host-bg daemon for this room (so the
    /// tab gets a `hosting=true` flag, which affects close-tab semantics).
    pub hosting: bool,
}

pub async fn run(opts: RunOpts) -> Result<()> {
    if !atty_check_stdout() {
        return Err(anyhow!(
            "TTY required — `cc-connect room` must run in an interactive terminal"
        ));
    }

    // ---- Fan-in channels --------------------------------------------------
    let (display_tx, mut display_rx) = mpsc::channel::<DisplayEvent>(256);
    let (pty_tx, mut pty_rx) = mpsc::channel::<PtyChunk>(256);
    let tab_io = TabIo {
        display_tx: display_tx.clone(),
        pty_tx: pty_tx.clone(),
    };

    // ---- App state --------------------------------------------------------
    let self_nick = std::fs::read_to_string(home_dir().join(".cc-connect").join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("self_nick")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty());
    let mut app = App::new(self_nick.clone());

    // ---- Initial tab ------------------------------------------------------
    let initial_pty_size = pane_size_for(current_terminal_size()?);
    let initial_id = app.tabs.alloc_id();
    let initial_tab = spawn_tab(
        initial_id,
        SpawnTabArgs {
            ticket: opts.ticket.clone(),
            no_relay: opts.no_relay,
            relay: opts.relay.clone(),
            claude_argv: opts.claude_argv.clone(),
            claude_cwd: opts.claude_cwd.clone(),
            initial_pty_size,
            hosting: opts.hosting,
        },
        &tab_io,
    )
    .await
    .context("spawn initial tab")?;
    let banner_topic = initial_tab.topic_short();
    let banner_ticket = initial_tab.ticket.clone();
    app.tabs.add(initial_tab);
    push_chat_to_active(
        &mut app,
        ChatLine::new(
            ChatLineKind::System,
            format!(
                "Room {} — Ctrl-N new tab, Ctrl-W close tab, F2 switch pane, Ctrl-Q quit",
                banner_topic
            ),
        ),
    );
    push_chat_to_active(
        &mut app,
        ChatLine::new(
            ChatLineKind::System,
            "Share this ticket to invite peers:".to_string(),
        ),
    );
    push_chat_to_active(
        &mut app,
        ChatLine::new(ChatLineKind::Marker, banner_ticket),
    );

    // ---- Terminal init ----------------------------------------------------
    let _term_guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).context("init terminal")?;

    // ---- Main loop --------------------------------------------------------
    let mut crossterm_events = EventStream::new();
    let claude_argv = opts.claude_argv.clone();
    let claude_cwd = opts.claude_cwd.clone();

    loop {
        // Apply the active tab's claude-pane scrollback offset to vt100
        // before drawing. set_scrollback is cheap (just a Screen field
        // mutation) — re-applying every frame keeps the rendered view in
        // sync with `claude_scroll` without any other plumbing.
        if let Some(t) = app.tabs.active_tab_mut() {
            t.vt_parser.screen_mut().set_scrollback(t.claude_scroll as usize);
        }
        terminal.draw(|f| draw(f, &app)).context("draw")?;
        if app.should_exit || app.tabs.is_empty() {
            break;
        }

        tokio::select! {
            ev = crossterm_events.next() => match ev {
                Some(Ok(CtEvent::Key(key))) if key.kind == KeyEventKind::Press => {
                    handle_key(&mut app, key, &tab_io, &claude_argv, &claude_cwd, opts.no_relay).await;
                }
                Some(Ok(CtEvent::Resize(cols, rows))) => {
                    let claude_size = pane_size_for(PtySize {
                        rows, cols, pixel_width: 0, pixel_height: 0,
                    });
                    for tab in app.tabs.tabs.values_mut() {
                        let _ = tab.pty_master.resize(claude_size);
                        tab.vt_parser.screen_mut().set_size(claude_size.rows, claude_size.cols);
                    }
                }
                Some(Err(_)) | None => break,
                _ => {}
            },
            ev = display_rx.recv() => match ev {
                Some((tid, line)) => {
                    apply_display_line(&mut app, tid, line);
                }
                None => break,
            },
            ev = pty_rx.recv() => match ev {
                Some((tid, bytes)) => {
                    if let Some(t) = app.tabs.get_mut(tid) {
                        t.vt_parser.process(&bytes);
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                // periodic redraw (cursor blink, etc.)
            }
        }
    }

    // Cleanup. RoomTab::Drop kills claude + aborts forwarders + drops chat handle.
    Ok(())
}

// ---------- helpers ---------------------------------------------------------

fn push_chat_to_active(app: &mut App, line: ChatLine) {
    if let Some(t) = app.tabs.active_tab_mut() {
        t.push_chat(line);
    }
}

fn apply_display_line(app: &mut App, tid: TabId, line: DisplayLine) {
    let line = match line {
        DisplayLine::System(s) => ChatLine::new(ChatLineKind::System, s),
        DisplayLine::Marker(s) => ChatLine::new(ChatLineKind::Marker, s),
        DisplayLine::Incoming { nick_short, body, mentions_me } => {
            let kind = if mentions_me {
                ChatLineKind::IncomingMention
            } else {
                ChatLineKind::Incoming
            };
            let prefix = if mentions_me { "(@me) " } else { "" };
            // Record the sender so the @-completion popup has it on tap.
            if let Some(t) = app.tabs.get_mut(tid) {
                t.record_nick(&nick_short);
            }
            ChatLine::new(kind, format!("{prefix}[{nick_short}] {body}"))
        }
        DisplayLine::Echo(s) => ChatLine::new(ChatLineKind::Echo, s),
        DisplayLine::Warn(s) => ChatLine::new(ChatLineKind::Warn, s),
    };
    if let Some(t) = app.tabs.get_mut(tid) {
        t.push_chat(line);
    }
}

// ---------- key dispatch ----------------------------------------------------

async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    tab_io: &TabIo,
    claude_argv: &[String],
    claude_cwd: &Option<PathBuf>,
    no_relay: bool,
) {
    // Overlay key dispatch takes precedence.
    if app.overlay.is_some() {
        handle_overlay_key(app, key, tab_io, claude_argv, claude_cwd, no_relay).await;
        return;
    }

    // Global hotkeys (regardless of focus).
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('q') => {
                app.should_exit = true;
                return;
            }
            KeyCode::Char('n') => {
                app.overlay = Some(Overlay::NewRoomPicker);
                return;
            }
            KeyCode::Char('w') => {
                close_active_tab(app);
                return;
            }
            KeyCode::Char('y') => {
                let ticket = app
                    .tabs
                    .active_tab()
                    .map(|t| t.ticket.clone())
                    .unwrap_or_default();
                if !ticket.is_empty() {
                    match arboard::Clipboard::new().and_then(|mut c| c.set_text(ticket.clone())) {
                        Ok(()) => push_chat_to_active(
                            app,
                            ChatLine::new(
                                ChatLineKind::System,
                                "✓ ticket copied to clipboard".to_string(),
                            ),
                        ),
                        Err(e) => {
                            push_chat_to_active(
                                app,
                                ChatLine::new(
                                    ChatLineKind::Warn,
                                    format!("clipboard unreachable ({e}); reprinting below"),
                                ),
                            );
                            push_chat_to_active(
                                app,
                                ChatLine::new(ChatLineKind::Marker, ticket),
                            );
                        }
                    }
                }
                return;
            }
            _ => {}
        }
    }

    // Numeric tab switch (1-9) + page-up/down for cycling.
    if key.modifiers.is_empty() {
        if let KeyCode::Char(c) = key.code {
            if let Some(d) = c.to_digit(10) {
                if d >= 1 && d <= 9 && app.focus != Focus::Claude {
                    app.tabs.switch_to_index((d - 1) as usize);
                    return;
                }
            }
        }
    }
    if key.code == KeyCode::F(2) {
        app.toggle_focus();
        return;
    }
    if key.code == KeyCode::Tab && key.modifiers.is_empty() && app.focus == Focus::Chat {
        app.toggle_focus();
        return;
    }
    if key.code == KeyCode::BackTab {
        app.toggle_focus();
        return;
    }

    // PageUp/PageDown scroll the focused pane regardless of focus.
    // Claude rarely needs PgUp/PgDn for its own UI, so it's safe to
    // intercept globally — the user gets a single, predictable scroll
    // gesture in either pane.
    match key.code {
        KeyCode::PageUp => {
            scroll_active_pane(app, -SCROLL_STEP);
            return;
        }
        KeyCode::PageDown => {
            scroll_active_pane(app, SCROLL_STEP);
            return;
        }
        _ => {}
    }

    match app.focus {
        Focus::Chat => handle_chat_key(app, key).await,
        Focus::Claude => handle_claude_key(app, key).await,
    }
}

/// One PgUp / PgDn nudges the active pane by this many lines (chat) or
/// rows (claude). Approximately a half-screen at typical terminal sizes.
const SCROLL_STEP: i32 = 10;

fn scroll_active_pane(app: &mut App, delta: i32) {
    let focus = app.focus;
    let Some(t) = app.tabs.active_tab_mut() else {
        return;
    };
    let target = match focus {
        Focus::Chat => &mut t.chat_scroll,
        Focus::Claude => &mut t.claude_scroll,
    };
    *target = if delta < 0 {
        target.saturating_add((-delta) as u16)
    } else {
        target.saturating_sub(delta as u16)
    };
}

async fn handle_chat_key(app: &mut App, key: KeyEvent) {
    let active_id = match app.tabs.active {
        Some(id) => id,
        None => return,
    };
    let self_nick = app.self_nick.clone();
    let tab = match app.tabs.get_mut(active_id) {
        Some(t) => t,
        None => return,
    };

    // Decide whether the @-completion popup is currently displayed.
    // Recompute every keystroke based on input_buf state; cheap and means
    // we never have to track open/close transitions.
    let popup_visible = !tab.mention_dismissed
        && crate::mention::current_at_token(&tab.input_buf)
            .map(|p| {
                !crate::mention::mention_candidates(
                    &tab.recent_nicks,
                    p,
                    self_nick.as_deref(),
                )
                .is_empty()
            })
            .unwrap_or(false);

    if popup_visible {
        // Refresh the candidate list now so we can dispatch on indexes.
        let token = crate::mention::current_at_token(&tab.input_buf).unwrap_or("");
        let candidates =
            crate::mention::mention_candidates(&tab.recent_nicks, token, self_nick.as_deref());
        let n = candidates.len();
        match key.code {
            KeyCode::Up => {
                tab.mention_idx = if tab.mention_idx == 0 {
                    n.saturating_sub(1)
                } else {
                    tab.mention_idx - 1
                };
                return;
            }
            KeyCode::Down => {
                tab.mention_idx = (tab.mention_idx + 1) % n.max(1);
                return;
            }
            KeyCode::Tab | KeyCode::Enter => {
                let pick = candidates.get(tab.mention_idx).cloned();
                if let Some(nick) = pick {
                    crate::mention::complete_at(&mut tab.input_buf, &nick);
                    tab.mention_idx = 0;
                }
                return;
            }
            KeyCode::Esc => {
                tab.mention_dismissed = true;
                tab.mention_idx = 0;
                return;
            }
            _ => {} // fall through to normal editing
        }
    }

    match key.code {
        KeyCode::Enter => {
            if !tab.input_buf.is_empty() {
                let line = std::mem::take(&mut tab.input_buf);
                tab.mention_idx = 0;
                tab.mention_dismissed = false;
                let _ = tab.chat_handle.input_tx.send(line).await;
            }
        }
        KeyCode::Backspace => {
            tab.input_buf.pop();
            tab.mention_idx = 0;
            tab.mention_dismissed = false;
        }
        KeyCode::Char(c) => {
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                tab.input_buf.push(c);
                tab.mention_idx = 0;
                tab.mention_dismissed = false;
            }
        }
        _ => {}
    }
}

async fn handle_claude_key(app: &mut App, key: KeyEvent) {
    let active_id = match app.tabs.active {
        Some(id) => id,
        None => return,
    };
    let tab = match app.tabs.get_mut(active_id) {
        Some(t) => t,
        None => return,
    };
    let bytes = encode_key(key);
    if bytes.is_empty() {
        return;
    }
    let _ = tab.pty_writer.write_all(&bytes);
    let _ = tab.pty_writer.flush();
}

async fn handle_overlay_key(
    app: &mut App,
    key: KeyEvent,
    tab_io: &TabIo,
    claude_argv: &[String],
    claude_cwd: &Option<PathBuf>,
    no_relay: bool,
) {
    let overlay = app.overlay.take();
    let mut next: Option<Overlay> = None;
    match overlay {
        Some(Overlay::NewRoomPicker) => match key.code {
            KeyCode::Char('j') | KeyCode::Char('J') => {
                next = Some(Overlay::JoinTicketPrompt { buf: String::new() });
            }
            KeyCode::Esc | KeyCode::Char('q') => {}
            _ => {
                next = Some(Overlay::NewRoomPicker);
            }
        },
        Some(Overlay::JoinTicketPrompt { mut buf }) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                let ticket = buf.trim().to_string();
                if !ticket.is_empty() {
                    let id = app.tabs.alloc_id();
                    let initial_pty_size = pane_size_for(current_terminal_size().unwrap_or(PtySize {
                        rows: 30, cols: 120, pixel_width: 0, pixel_height: 0,
                    }));
                    let args = SpawnTabArgs {
                        ticket,
                        no_relay,
                        relay: None,
                        claude_argv: claude_argv.to_vec(),
                        claude_cwd: claude_cwd.clone(),
                        initial_pty_size,
                        hosting: false,
                    };
                    match spawn_tab(id, args, tab_io).await {
                        Ok(tab) => {
                            let topic = tab.topic_short();
                            let ticket = tab.ticket.clone();
                            app.tabs.add(tab);
                            push_chat_to_active(
                                app,
                                ChatLine::new(
                                    ChatLineKind::System,
                                    format!("Joined room {topic}"),
                                ),
                            );
                            push_chat_to_active(
                                app,
                                ChatLine::new(ChatLineKind::Marker, ticket),
                            );
                        }
                        Err(e) => {
                            next = Some(Overlay::Notice(format!("join failed: {e:#}")));
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                buf.pop();
                next = Some(Overlay::JoinTicketPrompt { buf });
            }
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    buf.push(c);
                }
                next = Some(Overlay::JoinTicketPrompt { buf });
            }
            _ => {
                next = Some(Overlay::JoinTicketPrompt { buf });
            }
        },
        Some(Overlay::ConfirmCloseHost { topic_hex }) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Err(e) = cc_connect::host_bg::run_stop(&topic_hex) {
                    next = Some(Overlay::Notice(format!("stop daemon failed: {e:#}")));
                }
                drop_active_tab(app);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Enter => {
                drop_active_tab(app);
            }
            _ => {
                next = Some(Overlay::ConfirmCloseHost { topic_hex });
            }
        },
        Some(Overlay::Notice(s)) => match key.code {
            KeyCode::Esc | KeyCode::Enter => {}
            _ => next = Some(Overlay::Notice(s)),
        },
        None => {}
    }
    app.overlay = next;
}

fn close_active_tab(app: &mut App) {
    let (hosting, topic_hex) = match app.tabs.active_tab() {
        Some(t) => (t.hosting, t.topic_hex.clone()),
        None => return,
    };
    if hosting {
        app.overlay = Some(Overlay::ConfirmCloseHost { topic_hex });
    } else {
        drop_active_tab(app);
    }
}

fn drop_active_tab(app: &mut App) {
    if let Some(id) = app.tabs.active {
        app.tabs.remove(id);
    }
}

// ---------- rendering -------------------------------------------------------

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(1), // tab strip
            Constraint::Min(1),    // panes
        ])
        .split(area);
    draw_header(f, chunks[0], app);
    draw_tab_strip(f, chunks[1], app);
    if let Some(active) = app.tabs.active_tab() {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(chunks[2]);
        claude_pane::render(f, panes[0], active, app.focus == Focus::Claude);
        chat_pane::render(
            f,
            panes[1],
            active,
            app.focus == Focus::Chat,
            app.self_nick.as_deref(),
        );
    } else {
        let placeholder = Paragraph::new("No active tabs. Ctrl-N to open one, Ctrl-Q to quit.");
        f.render_widget(placeholder, chunks[2]);
    }
    draw_overlay(f, app);
}

fn draw_header(f: &mut Frame, area: Rect, _app: &App) {
    let label = " cc-connect ";
    let hint = " [1-9] tab  [Ctrl-N] new  [Ctrl-W] close  [F2/Tab] switch pane  [Ctrl-Y] copy ticket  [Ctrl-Q] quit ";
    let line = Line::from(vec![
        Span::styled(label, theme::header_chip()),
        Span::styled(hint, theme::header_hint()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_tab_strip(f: &mut Frame, area: Rect, app: &App) {
    if app.tabs.is_empty() {
        f.render_widget(Paragraph::new(""), area);
        return;
    }
    let mut spans: Vec<Span> = Vec::new();
    for (i, &id) in app.tabs.order.iter().enumerate() {
        let tab = match app.tabs.tabs.get(&id) {
            Some(t) => t,
            None => continue,
        };
        let active = app.tabs.active == Some(id);
        let label = format!(" [{n}] {short}{tag} ", n = i + 1, short = tab.topic_short(), tag = if tab.hosting { "·H" } else { "" });
        let style = if active {
            theme::tab_active()
        } else {
            theme::tab_inactive()
        };
        spans.push(Span::styled(label, style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_overlay(f: &mut Frame, app: &App) {
    let area = f.area();
    let Some(overlay) = &app.overlay else {
        return;
    };
    let (title, body, h) = match overlay {
        Overlay::NewRoomPicker => (
            " new room ",
            "[j] join existing room (paste ticket)\n[Esc] cancel".to_string(),
            5,
        ),
        Overlay::JoinTicketPrompt { buf } => (
            " join room ",
            format!("Paste ticket and press Enter:\n\n› {buf}"),
            6,
        ),
        Overlay::ConfirmCloseHost { topic_hex } => (
            " close tab ",
            format!(
                "You're hosting {}. Stop the host daemon too?\n  [y] yes — close tab + stop daemon\n  [n] no — close tab, daemon stays (default)",
                &topic_hex[..12.min(topic_hex.len())]
            ),
            6,
        ),
        Overlay::Notice(s) => (" notice ", format!("{s}\n\n[Esc] dismiss"), 6),
    };
    let popup = centered_rect(70, h, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme::BORDER_TYPE)
        .border_style(theme::border_focused())
        .title(Span::styled(title, theme::pane_title()));
    let inner = block.inner(popup);
    f.render_widget(Clear, popup);
    f.render_widget(block, popup);
    f.render_widget(Paragraph::new(body).style(theme::input_text()), inner);
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = area.width.saturating_mul(percent_x) / 100;
    let h = height.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width,
        height: h,
    }
}

// ---------- key encoding (unchanged from the v0.3 single-tab path) ----------

fn encode_key(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut buf = Vec::with_capacity(8);
    if alt {
        buf.push(0x1b);
    }
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let lower = c.to_ascii_lowercase();
                if ('a'..='z').contains(&lower) {
                    buf.push((lower as u8) - b'a' + 1);
                } else {
                    let mut tmp = [0u8; 4];
                    buf.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
                }
            } else {
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
            }
        }
        KeyCode::Enter => buf.push(b'\r'),
        KeyCode::Backspace => buf.push(0x7f),
        KeyCode::Tab => buf.push(b'\t'),
        KeyCode::BackTab => buf.extend_from_slice(b"\x1b[Z"),
        KeyCode::Esc => buf.push(0x1b),
        KeyCode::Up => buf.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => buf.extend_from_slice(b"\x1b[B"),
        KeyCode::Right => buf.extend_from_slice(b"\x1b[C"),
        KeyCode::Left => buf.extend_from_slice(b"\x1b[D"),
        KeyCode::Home => buf.extend_from_slice(b"\x1b[H"),
        KeyCode::End => buf.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => buf.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => buf.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => buf.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => buf.extend_from_slice(b"\x1b[2~"),
        _ => {}
    }
    buf
}

// ---------- terminal size + lifecycle helpers ------------------------------

fn current_terminal_size() -> Result<PtySize> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 30));
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn pane_size_for(full: PtySize) -> PtySize {
    let claude_cols = (full.cols as f32 * 0.60) as u16;
    let claude_cols = claude_cols.saturating_sub(2);
    let claude_rows = full.rows.saturating_sub(4); // header + tab-strip + 2 borders
    PtySize {
        cols: claude_cols.max(20),
        rows: claude_rows.max(5),
        pixel_width: 0,
        pixel_height: 0,
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        let mut out = std::io::stdout();
        execute!(out, EnterAlternateScreen, EnableMouseCapture)
            .context("enter alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut out = std::io::stdout();
        let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
    }
}

fn atty_check_stdout() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
}
