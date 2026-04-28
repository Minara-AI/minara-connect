//! Main async event loop: drives the TUI's redraw, dispatches keys, and
//! pumps bytes between the chat session and the embedded `claude` PTY.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use cc_connect::chat_session::{self, ChatHandle, ChatSessionConfig, DisplayLine};
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
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame, Terminal,
};
use tokio::sync::mpsc;

use crate::app::{App, ChatLineKind, Focus};
use crate::{chat_pane, claude_pane, theme};

/// Caller-supplied configuration for [`run`].
pub struct RunOpts {
    /// `cc1-…` ticket the chat session joins.
    pub ticket: String,
    /// 64-char hex topic id from the ticket. Set as `CC_CONNECT_ROOM` on
    /// the spawned `claude` so the hook scopes to this room only.
    pub topic_hex: String,
    pub no_relay: bool,
    pub relay: Option<String>,
    /// Argv for the embedded child. Default is `["claude"]`.
    pub claude_argv: Vec<String>,
    /// Cwd of the spawned `claude`. None → inherit.
    pub claude_cwd: Option<PathBuf>,
}

pub async fn run(opts: RunOpts) -> Result<()> {
    if !atty_check_stdout() {
        return Err(anyhow!(
            "TTY required — `cc-connect room` must run in an interactive terminal"
        ));
    }

    // ---- 1. Boot the chat session ------------------------------------------
    let cfg = ChatSessionConfig {
        ticket: opts.ticket.clone(),
        no_relay: opts.no_relay,
        relay: opts.relay.clone(),
    };
    let chat_handle = chat_session::spawn(cfg)
        .await
        .context("spawn chat_session")?;

    // ---- 2. Spawn `claude` over a PTY --------------------------------------
    // Use a sane initial size; we resize on the first crossterm Resize event.
    let pty_system = native_pty_system();
    let initial_size = current_terminal_size()?;
    let claude_pane_size = pane_size_for(initial_size, /* split = right pane */);
    let pair = pty_system
        .openpty(claude_pane_size)
        .map_err(|e| anyhow!("openpty: {e}"))?;

    let mut cmd = CommandBuilder::new(&opts.claude_argv[0]);
    for arg in opts.claude_argv.iter().skip(1) {
        cmd.arg(arg);
    }
    cmd.env("CC_CONNECT_ROOM", &opts.topic_hex);
    if let Some(cwd) = &opts.claude_cwd {
        cmd.cwd(cwd);
    }
    let mut pty_child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow!("spawn claude in PTY: {e}"))?;

    // Move the master end out so we can read/write asynchronously, and drop
    // the slave handle (the child has it).
    let pty_master = pair.master;
    let mut pty_writer = pty_master
        .take_writer()
        .map_err(|e| anyhow!("pty take_writer: {e}"))?;
    let mut pty_reader = pty_master
        .try_clone_reader()
        .map_err(|e| anyhow!("pty try_clone_reader: {e}"))?;

    // Pump PTY → mpsc on a blocking thread.
    let (pty_tx, mut pty_rx) = mpsc::channel::<Vec<u8>>(64);
    tokio::task::spawn_blocking(move || pty_reader_loop(&mut *pty_reader, pty_tx));

    // ---- 3. Initialise the terminal + App ----------------------------------
    let _term_guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).context("init terminal")?;
    let (init_rows, init_cols) = (claude_pane_size.rows, claude_pane_size.cols);

    let mut app = App::new(&opts.topic_hex, &opts.ticket, init_rows, init_cols);
    app.push_chat(
        ChatLineKind::System,
        format!("Room {} — Tab switches pane, Ctrl-Q quits, Ctrl-Y reprints ticket", &app.topic_short),
    );
    // Print the full ticket so the user can share it. ~250 chars, will wrap.
    app.push_chat(ChatLineKind::System, "Share this ticket to invite peers:".to_string());
    app.push_chat(ChatLineKind::Marker, opts.ticket.clone());

    // ---- 4. Run the loop ---------------------------------------------------
    let mut crossterm_events = EventStream::new();
    let mut chat_handle = chat_handle;
    let mut redraw = true;

    loop {
        if redraw {
            terminal
                .draw(|f| draw(f, &app))
                .context("terminal draw")?;
            redraw = false;
        }
        if app.should_exit {
            break;
        }

        tokio::select! {
            // ---- Crossterm: keyboard / resize ------------------------------
            ev = crossterm_events.next() => {
                match ev {
                    Some(Ok(CtEvent::Key(key))) => {
                        if key.kind == KeyEventKind::Press {
                            handle_key(&mut app, key, &mut chat_handle, &mut *pty_writer, &pty_master).await;
                            redraw = true;
                        }
                    }
                    Some(Ok(CtEvent::Resize(cols, rows))) => {
                        let claude_size = pane_size_for(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                        let _ = pty_master.resize(claude_size);
                        app.resize_pty_screen(claude_size.rows, claude_size.cols);
                        redraw = true;
                    }
                    Some(Err(_)) | None => break,
                    _ => {}
                }
            }

            // ---- chat_session display lines --------------------------------
            line = chat_handle.display_rx.recv() => {
                match line {
                    Some(DisplayLine::System(s)) => app.push_chat(ChatLineKind::System, s),
                    Some(DisplayLine::Marker(s)) => app.push_chat(ChatLineKind::Marker, s),
                    Some(DisplayLine::Incoming { nick_short, body }) => {
                        app.push_chat(ChatLineKind::Incoming, format!("[{nick_short}] {body}"));
                    }
                    Some(DisplayLine::Echo(s)) => app.push_chat(ChatLineKind::Echo, s),
                    Some(DisplayLine::Warn(s)) => app.push_chat(ChatLineKind::Warn, s),
                    None => {
                        app.push_chat(ChatLineKind::Warn, "[chat] session closed");
                        // Don't exit — let user inspect the scrollback. They Ctrl-Q out.
                    }
                }
                redraw = true;
            }

            // ---- PTY → vt100 -----------------------------------------------
            chunk = pty_rx.recv() => {
                match chunk {
                    Some(bytes) => {
                        app.feed_pty_bytes(&bytes);
                        redraw = true;
                    }
                    None => {
                        app.push_chat(ChatLineKind::Warn, "[claude] pane closed (child exited)");
                    }
                }
            }

            // Periodic tick — covers the case where vt100 cursor blink etc.
            // would otherwise be invisible. Cheap.
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                redraw = true;
            }
        }
    }

    // Cleanup: kill the claude child if it's still around. The chat session
    // shuts down when chat_handle's input_tx drops at the end of this scope.
    let _ = pty_child.kill();
    let _ = pty_child.wait();
    Ok(())
}

// ---- Drawing ---------------------------------------------------------------

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // header
            Constraint::Min(1),     // panes
        ])
        .split(area);
    draw_header(f, chunks[0], app);

    // claude on the left (wide, code-friendly), chat on the right (narrow).
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[1]);
    claude_pane::render(f, panes[0], app);
    chat_pane::render(f, panes[1], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let label = format!(" cc-connect · room {} ", app.topic_short);
    let hint = " [F2 / Tab] switch pane   [Ctrl-Y] copy ticket   [Ctrl-Q] quit ";
    let line = Line::from(vec![
        Span::styled(label, theme::header_chip()),
        Span::styled(hint, theme::header_hint()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

// ---- Key dispatch ----------------------------------------------------------

async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    chat: &mut ChatHandle,
    pty_writer: &mut dyn Write,
    pty_master: &Box<dyn MasterPty + Send>,
) {
    let _ = pty_master; // silence unused-arg warn — kept for symmetry / future use
    // Global hotkeys, regardless of focus.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('q') => {
                app.should_exit = true;
                return;
            }
            KeyCode::Char('y') => {
                // Copy the ticket to the system clipboard. Fall back to
                // re-printing it in scrollback if the clipboard is unreachable
                // (headless Linux, locked-down macOS sandbox, etc.).
                match arboard::Clipboard::new()
                    .and_then(|mut c| c.set_text(app.ticket.clone()))
                {
                    Ok(()) => {
                        app.push_chat(
                            ChatLineKind::System,
                            "✓ ticket copied to clipboard".to_string(),
                        );
                    }
                    Err(e) => {
                        app.push_chat(
                            ChatLineKind::Warn,
                            format!("clipboard unreachable ({e}); reprinting ticket below"),
                        );
                        let ticket = app.ticket.clone();
                        app.push_chat(ChatLineKind::Marker, ticket);
                    }
                }
                return;
            }
            KeyCode::Char('c') if app.focus == Focus::Chat => {
                // Ctrl-C in chat pane = quit (mirrors `cc-connect chat` REPL).
                app.should_exit = true;
                return;
            }
            _ => {}
        }
    }
    // F2 is the global "switch focus" key — works from BOTH panes, doesn't
    // collide with anything Claude Code uses. Tab from chat is a one-way
    // convenience (Tab inside Claude is autocomplete; we forward it).
    if key.code == KeyCode::F(2) {
        app.toggle_focus();
        return;
    }
    if key.code == KeyCode::Tab && key.modifiers.is_empty() && app.focus == Focus::Chat {
        app.toggle_focus();
        return;
    }
    if key.code == KeyCode::BackTab {
        // Shift-Tab from anywhere swaps focus too.
        app.toggle_focus();
        return;
    }

    match app.focus {
        Focus::Chat => handle_chat_key(app, key, chat).await,
        Focus::Claude => handle_claude_key(key, pty_writer),
    }
}

async fn handle_chat_key(app: &mut App, key: KeyEvent, chat: &mut ChatHandle) {
    match key.code {
        KeyCode::Enter => {
            if !app.input_buf.is_empty() {
                let line = std::mem::take(&mut app.input_buf);
                let _ = chat.input_tx.send(line).await;
            }
        }
        KeyCode::Backspace => {
            app.input_buf.pop();
        }
        KeyCode::Char(c) => {
            // Filter Ctrl-modified printables — they're hotkeys, not input.
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                app.input_buf.push(c);
            }
        }
        _ => {}
    }
}

fn handle_claude_key(key: KeyEvent, pty_writer: &mut dyn Write) {
    let bytes = encode_key(key);
    if bytes.is_empty() {
        return;
    }
    let _ = pty_writer.write_all(&bytes);
    let _ = pty_writer.flush();
}

/// Translate a crossterm KeyEvent into the byte sequence a real terminal
/// would send to its child. Covers the common cases — chars, Enter,
/// Backspace, Tab, Esc, arrows. Modifiers handle Ctrl-letter encodings.
fn encode_key(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut buf = Vec::with_capacity(8);
    if alt {
        buf.push(0x1b); // ESC prefix for Alt-X.
    }
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Ctrl-A..Ctrl-Z → 0x01..0x1A.
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

// ---- PTY plumbing ----------------------------------------------------------

fn pty_reader_loop(reader: &mut dyn Read, tx: mpsc::Sender<Vec<u8>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.blocking_send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn current_terminal_size() -> Result<PtySize> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 30));
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

/// Compute the size for the right pane (claude). We allocate 60% of the
/// width to it (matching the layout in `draw`), and the full inner height
/// minus the header row + 2 borders.
fn pane_size_for(full: PtySize) -> PtySize {
    let claude_cols = (full.cols as f32 * 0.60) as u16;
    let claude_cols = claude_cols.saturating_sub(2); // borders
    let claude_rows = full.rows.saturating_sub(3); // header + 2 borders
    PtySize {
        cols: claude_cols.max(20),
        rows: claude_rows.max(5),
        pixel_width: 0,
        pixel_height: 0,
    }
}

// ---- Terminal lifecycle ----------------------------------------------------

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
