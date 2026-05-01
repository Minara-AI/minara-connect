//! Right pane (in the new claude-left layout): chat scrollback + a one-line
//! input box at the bottom. Operates on the currently-active [`RoomTab`].

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::ChatLineKind;
use crate::mention;
use crate::tabs::RoomTab;
use crate::theme;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    tab: &RoomTab,
    focused: bool,
    self_nick: Option<&str>,
) {
    let border_style = if focused {
        theme::border_focused()
    } else {
        theme::border_unfocused()
    };

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(theme::BORDER_TYPE)
        .border_style(border_style)
        .title(Span::styled(
            format!(" 💬 chat · {} ", tab.topic_short()),
            theme::pane_title(),
        ));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // chat-ui parity: peer messages render `[nick] body` left-aligned;
    // own messages render `body [nick]` right-aligned so the nick
    // anchors against the right edge (user-requested layout). System,
    // marker, warn lines render full-width left-aligned.
    let mut lines: Vec<Line> = Vec::with_capacity(tab.chat_lines.len() * 2);
    for cl in tab.chat_lines.iter() {
        let is_peer = matches!(
            cl.kind,
            ChatLineKind::Incoming | ChatLineKind::IncomingMention
        );
        let is_own = matches!(cl.kind, ChatLineKind::Echo);
        let align = if is_own {
            Alignment::Right
        } else {
            Alignment::Left
        };

        let main = match cl.kind {
            ChatLineKind::System => Line::from(Span::styled(cl.text.clone(), theme::chat_system())),
            ChatLineKind::Marker => Line::from(Span::styled(cl.text.clone(), theme::chat_marker())),
            ChatLineKind::Incoming => render_incoming(&cl.text, false, false),
            ChatLineKind::IncomingMention => render_incoming(&cl.text, true, false),
            ChatLineKind::Echo => render_incoming(&cl.text, false, true),
            ChatLineKind::Warn => Line::from(Span::styled(cl.text.clone(), theme::chat_warn())),
        };
        lines.push(main.alignment(align));

        // Per-message timestamp on the same side as its message body.
        if (is_peer || is_own) && cl.ts > 0 {
            let stamp = format!("{} Z", format_utc_hhmm(cl.ts));
            lines.push(Line::from(Span::styled(stamp, theme::chat_timestamp())).alignment(align));
        }
    }

    // Scroll position. `chat_scroll` is "rows back from bottom" so 0
    // tails the live feed and PgUp grows it. The Paragraph wraps long
    // lines via Wrap{trim:false} which inflates row count above
    // `lines.len()`; counting raw lines made PgUp dead-end before the
    // actual top. Estimate visual rows by ceil(line_width / area_width)
    // per line and use that as max_offset so PgUp can reach every row.
    let visible = chunks[0].height;
    let wrap_width = chunks[0].width.max(1);
    let mut visual_rows: u32 = 0;
    for line in &lines {
        let w = line.width() as u32;
        let rows = if w == 0 {
            1
        } else {
            w.div_ceil(wrap_width as u32)
        };
        visual_rows = visual_rows.saturating_add(rows);
    }
    let total = visual_rows.min(u16::MAX as u32) as u16;
    let max_offset = total.saturating_sub(visible);
    let scroll_y = max_offset.saturating_sub(tab.chat_scroll);
    let scrollback = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_y, 0));
    frame.render_widget(scrollback, chunks[0]);

    // Single-line input. When the buffer is wider than the visible
    // input area, scroll horizontally so the cursor (= end of buf)
    // stays in view. Without this the user types past the right edge
    // and ratatui silently truncates from the head — they keep typing
    // without seeing what they're writing.
    let prompt = if focused { "› " } else { "  " };
    let inner_width = chunks[1].width as usize;
    let prompt_w = prompt.chars().count();
    let cursor_w = 1; // one-cell space we leave for the live cursor
    let body_budget = inner_width.saturating_sub(prompt_w + cursor_w);
    let buf_chars: Vec<char> = tab.input_buf.chars().collect();
    let visible_buf: String = if buf_chars.len() <= body_budget {
        tab.input_buf.clone()
    } else {
        let start = buf_chars.len() - body_budget;
        buf_chars[start..].iter().collect()
    };
    let input = Paragraph::new(Line::from(vec![
        Span::styled(prompt, theme::input_prompt(focused)),
        Span::styled(visible_buf, theme::input_text()),
    ]));
    frame.render_widget(input, chunks[1]);

    // @-mention completion popup. Floats just above the input line, only
    // when the chat pane is focused, the user has an in-progress @-token,
    // they haven't pressed Esc to dismiss, and we have at least one match.
    if focused && !tab.mention_dismissed {
        if let Some(prefix) = mention::current_at_token(&tab.input_buf) {
            let candidates = mention::mention_candidates(&tab.recent_nicks, prefix, self_nick);
            if !candidates.is_empty() {
                render_mention_popup(frame, chunks[1], &candidates, tab.mention_idx);
            }
        }
    }
}

/// Tiny floating list anchored on top of the input line. Sized to fit
/// up to 5 entries; if there are more, the rest are hidden (the user
/// keeps typing to filter).
fn render_mention_popup(frame: &mut Frame, input_area: Rect, candidates: &[String], idx: usize) {
    const MAX_ROWS: u16 = 5;
    let visible_n = candidates.len().min(MAX_ROWS as usize);
    let height = (visible_n as u16) + 2; // +2 for top/bottom border
                                         // Width: longest candidate + 4 (border + padding + ↩ marker).
    let widest = candidates
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(0);
    let width = ((widest + 4) as u16).clamp(12, input_area.width);

    // Anchor flush left, just above the input line.
    let y = input_area.y.saturating_sub(height);
    let x = input_area.x;
    let popup = Rect::new(x, y, width, height);

    let items: Vec<ListItem> = candidates
        .iter()
        .take(MAX_ROWS as usize)
        .enumerate()
        .map(|(i, c)| {
            let style = if i == idx {
                Style::default()
                    .add_modifier(Modifier::REVERSED)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(format!("@{c}")).style(style)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme::BORDER_TYPE)
        .title(Span::styled(
            " mention · ↑↓ Tab/⏎ Esc ",
            theme::pane_title(),
        ));
    let list = List::new(items).block(block);
    frame.render_widget(Clear, popup);
    frame.render_widget(list, popup);
}

/// Epoch ms → `HH:MM` UTC. Same arithmetic as cc-connect-core's
/// `hook_format::format_utc_hhmm`, duplicated here to avoid making it
/// pub for one caller.
fn format_utc_hhmm(ts: i64) -> String {
    let total_minutes = ts.div_euclid(60_000);
    let day_minute = total_minutes.rem_euclid(1440);
    let hh = day_minute / 60;
    let mm = day_minute % 60;
    format!("{hh:02}:{mm:02}")
}

/// Render a `[<nick>] <body>` chat line. When `mention` is true the body
/// uses the brighter mention palette and a leading `(@me)` marker is
/// pulled out and styled separately. When `own` is true (echo of our
/// own send) the nick is anchored to the **right** of the body so the
/// user-requested layout reads `body  [nick]` instead of the
/// peer-style `[nick] body`. Own messages render in the accent palette
/// (chat-ui parity), peers in the incoming palette.
fn render_incoming(text: &str, mention: bool, own: bool) -> Line<'static> {
    let (nick_style, body_style) = if mention {
        (theme::chat_mention_nick(), theme::chat_mention_body())
    } else if own {
        (theme::chat_own_nick(), theme::chat_own_body())
    } else {
        (theme::chat_incoming_nick(), theme::chat_incoming_body())
    };
    let (mention_marker, rest_text) = if let Some(rest) = text.strip_prefix("(@me) ") {
        ("(@me) ", rest)
    } else {
        ("", text)
    };
    if let Some(rest) = rest_text.strip_prefix('[') {
        if let Some(close) = rest.find("] ") {
            let nick = &rest[..close];
            let body = &rest[close + 2..];
            let mut spans = Vec::with_capacity(6);
            if !mention_marker.is_empty() {
                spans.push(Span::styled(
                    mention_marker.to_string(),
                    theme::chat_mention_marker(),
                ));
            }
            if own {
                // body  [nick]  — nick anchored on the right edge.
                spans.push(Span::styled(body.to_string(), body_style));
                spans.push(Span::styled("  [".to_string(), nick_style));
                spans.push(Span::styled(nick.to_string(), nick_style));
                spans.push(Span::styled("]".to_string(), nick_style));
            } else {
                // [nick] body — peer-style, nick on the left.
                spans.push(Span::styled("[".to_string(), nick_style));
                spans.push(Span::styled(nick.to_string(), nick_style));
                spans.push(Span::styled("] ".to_string(), nick_style));
                spans.push(Span::styled(body.to_string(), body_style));
            }
            return Line::from(spans);
        }
    }
    Line::from(Span::styled(rest_text.to_string(), body_style))
}
