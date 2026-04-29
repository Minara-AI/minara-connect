//! TUI app state, multi-tab edition.
//!
//! - One [`crate::tabs::RoomTab`] per joined room. Each owns its own
//!   chat session + claude PTY + vt100 parser + scrollback.
//! - The `App` struct is the tab manager + overlay state + global flags.

use crate::tabs::TabSet;

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
    /// Epoch millis when this line was pushed. Drives the per-message
    /// `HH:MM` separator the chat pane renders below content lines so
    /// neighbouring messages stop visually fusing.
    pub ts: i64,
}

impl ChatLine {
    pub fn new(kind: ChatLineKind, text: String) -> Self {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self { kind, text, ts }
    }
}

/// Visual style hint for a chat line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatLineKind {
    System,
    Marker,
    Incoming,
    /// Incoming line that mentions me (`@<self_nick>`, `@cc`, `@claude`,
    /// `@all`, `@here`). Renders with a louder colour + a leading marker.
    IncomingMention,
    Echo,
    Warn,
}

/// Modal overlay drawn on top of the main view.
#[derive(Debug)]
pub enum Overlay {
    /// "[j]oin existing  [h]ost new" picker for Ctrl-N.
    NewRoomPicker,
    /// Text input for a ticket on the join path.
    JoinTicketPrompt {
        buf: String,
    },
    /// "Stop the host daemon too? [y/N]" while closing a hosted tab.
    ConfirmCloseHost {
        topic_hex: String,
    },
    /// Status / error popup with a message; dismiss with Esc.
    Notice(String),
}

/// All shared TUI state.
pub struct App {
    pub tabs: TabSet,
    pub focus: Focus,
    pub overlay: Option<Overlay>,
    pub should_exit: bool,
    pub self_nick: Option<String>,
    pub status: String,
}

impl App {
    pub fn new(self_nick: Option<String>) -> Self {
        Self {
            tabs: TabSet::new(),
            focus: Focus::Chat,
            overlay: None,
            should_exit: false,
            self_nick,
            status: String::new(),
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Chat => Focus::Claude,
            Focus::Claude => Focus::Chat,
        };
    }
}
