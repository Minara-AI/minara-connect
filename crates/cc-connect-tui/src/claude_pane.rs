//! Right pane: the embedded `claude` PTY rendered through tui-term.

use ratatui::{
    layout::Rect,
    text::Span,
    widgets::{Block, Borders},
    Frame,
};
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Focus};
use crate::theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Claude;
    let border_style = if focused {
        theme::border_focused()
    } else {
        theme::border_unfocused()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme::BORDER_TYPE)
        .border_style(border_style)
        .title(Span::styled(" 🤖 claude ", theme::pane_title()));
    let screen = app.vt_parser.screen();
    let widget = PseudoTerminal::new(screen).block(block);
    frame.render_widget(widget, area);
}
