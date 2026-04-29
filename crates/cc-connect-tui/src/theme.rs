//! Catppuccin Mocha palette + the small set of role-tagged colours we use.
//!
//! See <https://catppuccin.com/palette> for the canonical Mocha values.
//! Picking a community-maintained, well-tested palette beats freelancing
//! a colour scheme; readers/contributors with the Catppuccin theme on
//! their editor see consistent colours across panes.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

// Catppuccin Mocha — dark variant.
pub const ROSEWATER: Color = Color::Rgb(0xf5, 0xe0, 0xdc);
pub const PINK: Color = Color::Rgb(0xf5, 0xc2, 0xe7);
pub const MAUVE: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
pub const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);
pub const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
pub const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
pub const TEAL: Color = Color::Rgb(0x94, 0xe2, 0xd5);
pub const SKY: Color = Color::Rgb(0x89, 0xdc, 0xeb);
pub const BLUE: Color = Color::Rgb(0x89, 0xb4, 0xfa);
pub const LAVENDER: Color = Color::Rgb(0xb4, 0xbe, 0xfe);
pub const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub const SUBTEXT1: Color = Color::Rgb(0xba, 0xc2, 0xde);
pub const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);
pub const OVERLAY1: Color = Color::Rgb(0x7f, 0x84, 0x9c);
pub const SURFACE2: Color = Color::Rgb(0x58, 0x5b, 0x70);
pub const SURFACE0: Color = Color::Rgb(0x31, 0x32, 0x44);
pub const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
pub const CRUST: Color = Color::Rgb(0x11, 0x11, 0x1b);

// ---- Role mappings used throughout the TUI --------------------------------

pub const BORDER_TYPE: BorderType = BorderType::Rounded;

pub fn border_focused() -> Style {
    Style::default().fg(MAUVE)
}

pub fn border_unfocused() -> Style {
    Style::default().fg(SURFACE2)
}

pub fn pane_title() -> Style {
    Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD)
}

pub fn header_chip() -> Style {
    Style::default()
        .bg(MAUVE)
        .fg(CRUST)
        .add_modifier(Modifier::BOLD)
}

pub fn header_hint() -> Style {
    Style::default().fg(OVERLAY1).add_modifier(Modifier::ITALIC)
}

pub fn chat_system() -> Style {
    Style::default().fg(SKY).add_modifier(Modifier::BOLD)
}

pub fn chat_marker() -> Style {
    Style::default().fg(SUBTEXT0).add_modifier(Modifier::ITALIC)
}

pub fn chat_incoming_nick() -> Style {
    Style::default().fg(BLUE).add_modifier(Modifier::BOLD)
}

pub fn chat_incoming_body() -> Style {
    Style::default().fg(TEXT)
}

pub fn chat_echo() -> Style {
    Style::default().fg(GREEN)
}

pub fn chat_warn() -> Style {
    Style::default().fg(YELLOW)
}

// Mention-of-me styling: louder than regular incoming, with a bright pink
// (@me) marker on the left margin.
pub fn chat_mention_marker() -> Style {
    Style::default().fg(PINK).add_modifier(Modifier::BOLD)
}
pub fn chat_mention_nick() -> Style {
    Style::default().fg(PEACH).add_modifier(Modifier::BOLD)
}
pub fn chat_mention_body() -> Style {
    Style::default().fg(ROSEWATER).add_modifier(Modifier::BOLD)
}

pub fn input_prompt(focused: bool) -> Style {
    if focused {
        Style::default().fg(PEACH).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SURFACE2)
    }
}

pub fn input_text() -> Style {
    Style::default().fg(TEXT)
}

// Silence lints for palette colours we don't currently use but want
// available for future tweaks (Rosewater, Pink, Red, Teal, Surface0, Base).
#[allow(dead_code)]
const _UNUSED_PALETTE: [Color; 6] = [ROSEWATER, PINK, RED, TEAL, SURFACE0, BASE];
#[allow(dead_code)]
const _UNUSED_SUBTEXT: [Color; 1] = [SUBTEXT1];
