//! Right pane: the embedded `claude` PTY rendered through tui-term.
//!
//! The vt100 parser (owned by [`crate::app::App`]) holds the screen state.
//! We feed bytes into it from the PTY reader on the event loop, then render
//! the screen here every tick.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders},
    Frame,
};
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Focus};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Claude;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            " claude code ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let screen = app.vt_parser.screen();
    let widget = PseudoTerminal::new(screen).block(block);
    frame.render_widget(widget, area);
}
