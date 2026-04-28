//! In-process state shared between the render loop and the input handler.
//!
//! Two panes:
//!  - `Chat`  — native ratatui rendering: scrollback + input textbox.
//!  - `Claude` — vt100-emulated screen fed from the PTY's stdout, rendered
//!    via `tui_term::widget::PseudoTerminal`.

use std::collections::VecDeque;

/// Which pane currently has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Chat,
    Claude,
}

/// One line of chat scrollback. Rendered in the left pane.
#[derive(Debug, Clone)]
pub struct ChatLine {
    pub kind: ChatLineKind,
    pub text: String,
}

/// Visual style hint for a chat line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatLineKind {
    System,
    Marker,
    Incoming,
    Echo,
    Warn,
}

/// All TUI state. The render loop pulls from this; the event loop pushes.
pub struct App {
    pub focus: Focus,
    pub topic_short: String,
    pub ticket: String,
    pub chat_lines: VecDeque<ChatLine>,
    pub input_buf: String,
    pub vt_parser: vt100::Parser,
    /// Set true to exit the render loop next tick.
    pub should_exit: bool,
    /// Last status banner shown in the header (peer count etc.).
    pub status: String,
}

const CHAT_SCROLLBACK_CAP: usize = 1024;

impl App {
    pub fn new(topic_hex: &str, ticket: &str, rows: u16, cols: u16) -> Self {
        let topic_short = topic_hex
            .chars()
            .take(12.min(topic_hex.len()))
            .collect::<String>();
        Self {
            focus: Focus::Chat,
            topic_short,
            ticket: ticket.to_string(),
            chat_lines: VecDeque::new(),
            input_buf: String::new(),
            vt_parser: vt100::Parser::new(rows, cols, 0),
            should_exit: false,
            status: String::new(),
        }
    }

    pub fn push_chat(&mut self, kind: ChatLineKind, text: impl Into<String>) {
        if self.chat_lines.len() >= CHAT_SCROLLBACK_CAP {
            self.chat_lines.pop_front();
        }
        self.chat_lines.push_back(ChatLine {
            kind,
            text: text.into(),
        });
    }

    /// Feed a chunk of bytes from the PTY into the vt100 parser.
    pub fn feed_pty_bytes(&mut self, bytes: &[u8]) {
        self.vt_parser.process(bytes);
    }

    /// Resize the embedded terminal screen. Call when the claude-pane area
    /// changes due to a window resize.
    pub fn resize_pty_screen(&mut self, rows: u16, cols: u16) {
        self.vt_parser.screen_mut().set_size(rows, cols);
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Chat => Focus::Claude,
            Focus::Claude => Focus::Chat,
        };
    }
}
