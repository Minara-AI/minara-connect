//! Left pane: chat scrollback + a one-line input box at the bottom.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, ChatLineKind, Focus};
use crate::theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Chat;
    let border_style = if focused {
        theme::border_focused()
    } else {
        theme::border_unfocused()
    };

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(theme::BORDER_TYPE)
        .border_style(border_style)
        .title(Span::styled(" 💬 chat ", theme::pane_title()));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Split inner: scrollback above, input row at the bottom.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // ---- Scrollback ---------------------------------------------------------
    // Tail-N: render the last `chunks[0].height` rendered lines. Wrap is on
    // so longer messages (e.g. the ticket) span multiple rows.
    let lines: Vec<Line> = app
        .chat_lines
        .iter()
        .map(|cl| match cl.kind {
            ChatLineKind::System => Line::from(Span::styled(cl.text.clone(), theme::chat_system())),
            ChatLineKind::Marker => Line::from(Span::styled(cl.text.clone(), theme::chat_marker())),
            ChatLineKind::Incoming => render_incoming(&cl.text, false),
            ChatLineKind::IncomingMention => render_incoming(&cl.text, true),
            ChatLineKind::Echo => Line::from(Span::styled(cl.text.clone(), theme::chat_echo())),
            ChatLineKind::Warn => Line::from(Span::styled(cl.text.clone(), theme::chat_warn())),
        })
        .collect();

    let scrollback = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(scrollback, chunks[0]);

    // ---- Input row ----------------------------------------------------------
    let prompt = if focused { "› " } else { "  " };
    let input = Paragraph::new(Line::from(vec![
        Span::styled(prompt, theme::input_prompt(focused)),
        Span::styled(app.input_buf.as_str(), theme::input_text()),
    ]));
    frame.render_widget(input, chunks[1]);
}

/// "[<nick>] <body>" → distinct nick / body styles. When `mention` is true,
/// the body is rendered in a brighter mention colour and the leading "(@me)"
/// (already in the text from event_loop) keeps its bold mention style.
fn render_incoming(text: &str, mention: bool) -> Line<'static> {
    let (nick_style, body_style) = if mention {
        (theme::chat_mention_nick(), theme::chat_mention_body())
    } else {
        (theme::chat_incoming_nick(), theme::chat_incoming_body())
    };
    // Strip the `(@me) ` prefix added in event_loop, render it as its own
    // bright span so it pops on the left margin.
    let (mention_marker, rest_text) = if let Some(rest) = text.strip_prefix("(@me) ") {
        ("(@me) ", rest)
    } else {
        ("", text)
    };
    if let Some(rest) = rest_text.strip_prefix('[') {
        if let Some(close) = rest.find("] ") {
            let nick = &rest[..close];
            let body = &rest[close + 2..];
            let mut spans = Vec::with_capacity(5);
            if !mention_marker.is_empty() {
                spans.push(Span::styled(mention_marker.to_string(), theme::chat_mention_marker()));
            }
            spans.push(Span::styled("[".to_string(), nick_style));
            spans.push(Span::styled(nick.to_string(), nick_style));
            spans.push(Span::styled("] ".to_string(), nick_style));
            spans.push(Span::styled(body.to_string(), body_style));
            return Line::from(spans);
        }
    }
    Line::from(Span::styled(rest_text.to_string(), body_style))
}
