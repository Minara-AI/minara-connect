//! Left pane: chat scrollback + a one-line input box at the bottom.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, ChatLineKind, Focus};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Chat;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            " chat ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Split inner: scrollback above, input row at the bottom.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // ---- Scrollback ---------------------------------------------------------
    // Tail-N: render the last `chunks[0].height` rendered lines. Wrap is on
    // so longer messages span multiple rows.
    let lines: Vec<Line> = app
        .chat_lines
        .iter()
        .map(|cl| {
            let style = match cl.kind {
                ChatLineKind::System => Style::default().fg(Color::Cyan),
                ChatLineKind::Marker => Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
                ChatLineKind::Incoming => Style::default().fg(Color::White),
                ChatLineKind::Echo => Style::default().fg(Color::Green),
                ChatLineKind::Warn => Style::default().fg(Color::Yellow),
            };
            Line::from(Span::styled(cl.text.clone(), style))
        })
        .collect();

    let scrollback = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(scrollback, chunks[0]);

    // ---- Input row ----------------------------------------------------------
    let prompt = if focused { "› " } else { "  " };
    let input_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let input = Paragraph::new(Line::from(vec![
        Span::styled(prompt, input_style),
        Span::raw(app.input_buf.as_str()),
    ]));
    frame.render_widget(input, chunks[1]);
}
