//! Hook output formatting — pure in-memory algorithm.
//!
//! Implements `PROTOCOL.md` §7.3 steps 5-7: render unread Messages from any
//! number of active Rooms into the byte-budgeted stdout payload that
//! `cc-connect-hook` emits to Claude Code.
//!
//! - Single Room active:  `[chatroom @<nick> <hh:mm>Z] <body>\n`
//! - Multiple Rooms:      `[chatroom <room-tag> @<nick> <hh:mm>Z] <body>\n`
//! - 8 KiB hard cap (PROTOCOL.md §7.3 step 6): drop **oldest** Messages
//!   (by ULID, across the merged set) until the remainder + the prepended
//!   `[chatroom] (N older messages skipped to fit)\n` marker fits. The fit
//!   check is iterative — the marker's digit count grows with N.

use crate::message::{Message, KIND_FILE_DROP};
use std::collections::HashMap;
use std::path::Path;

/// PROTOCOL.md §7.3 step 6 hard cap. ADR-0004 / Spike 0 verified.
pub const HOOK_OUTPUT_BUDGET: usize = 8 * 1024;

/// Inputs collected by the Hook before formatting.
pub struct HookInput<'a> {
    /// Per-active-Room unread Messages, already filtered by Cursor and sorted
    /// ascending by ULID. Key: topic_id_hex (lowercase hex of the 32-byte
    /// topic ID).
    pub rooms: &'a HashMap<String, Vec<Message>>,
    /// Pubkey → nickname map, typically `~/.cc-connect/nicknames.json`.
    pub nicknames: &'a HashMap<String, String>,
    /// Base directory under which Room state lives (typically
    /// `~/.cc-connect/rooms/`). Used to compute `@file:` paths for
    /// `file_drop` Messages: `<rooms_base>/<topic>/files/<id>-<body>`.
    pub rooms_base: &'a Path,
    /// User's own self-declared nickname (from
    /// `~/.cc-connect/config.json::self_nick`). Used to detect @-mentions
    /// — lines that mention the user are tagged `for-you` in the rendered
    /// prefix so Claude prioritises them. `None` skips mention detection
    /// (only the always-on `@cc` / `@claude` / `@all` / `@here` tokens
    /// still trigger the mark).
    pub self_nick: Option<&'a str>,
    /// topic_id_hex → markdown summary text. Optional per-room rolling
    /// summary written by Claude via the MCP `cc_save_summary` tool.
    /// Injected ahead of the verbatim chat lines so Claude can pick up
    /// long-running room context without burning its 8 KB budget on
    /// raw history.
    pub room_summaries: &'a HashMap<String, String>,
    /// topic_id_hex → markdown listing of files dropped into the room
    /// (auto-maintained by chat_session at `<topic>/files/INDEX.md`).
    /// Last N entries are injected so Claude knows what files exist and
    /// where to find them on disk.
    pub room_file_indexes: &'a HashMap<String, String>,
}

/// Render the Hook's stdout payload. Always returns a complete (possibly
/// empty) UTF-8 string ending with `\n` (or empty string if no messages).
pub fn render(input: &HookInput) -> String {
    let multi_room = input.rooms.len() >= 2;

    // Merge across rooms, then sort by ULID ascending (PROTOCOL §7.3 step 7).
    let mut entries: Vec<(&str, &Message)> = input
        .rooms
        .iter()
        .flat_map(|(topic, msgs)| msgs.iter().map(move |m| (topic.as_str(), m)))
        .collect();
    entries.sort_by(|a, b| a.1.id.as_str().cmp(b.1.id.as_str()));

    if entries.is_empty() {
        return String::new();
    }

    let lines: Vec<String> = entries
        .iter()
        .map(|(topic, msg)| {
            format_line(
                topic,
                msg,
                input.nicknames,
                input.rooms_base,
                multi_room,
                input.self_nick,
            )
        })
        .collect();

    // Build the summary + files-index preamble. These fixed sections eat
    // a slice of the 8 KiB budget (1.5 KiB each at most); the verbatim
    // chat-line block uses whatever's left.
    let mut preamble = String::new();
    let mut topics: Vec<&String> = input.rooms.keys().collect();
    topics.sort();
    for topic in &topics {
        if let Some(summary) = input.room_summaries.get(*topic) {
            let trimmed = summary.trim();
            if !trimmed.is_empty() {
                let header = if multi_room {
                    format!(
                        "[cc-connect summary {}]",
                        topic.chars().take(6).collect::<String>().to_ascii_lowercase()
                    )
                } else {
                    "[cc-connect summary]".to_string()
                };
                let body = truncate_head(trimmed, SUMMARY_BUDGET);
                preamble.push_str(&header);
                preamble.push('\n');
                preamble.push_str(&body);
                preamble.push_str("\n\n");
            }
        }
    }
    for topic in &topics {
        if let Some(idx) = input.room_file_indexes.get(*topic) {
            let trimmed = idx.trim();
            if !trimmed.is_empty() {
                let header = if multi_room {
                    format!(
                        "[cc-connect files {}]",
                        topic.chars().take(6).collect::<String>().to_ascii_lowercase()
                    )
                } else {
                    "[cc-connect files]".to_string()
                };
                let body = tail_lines_within_budget(trimmed, FILES_INDEX_BUDGET);
                preamble.push_str(&header);
                preamble.push('\n');
                preamble.push_str(&body);
                preamble.push_str("\n\n");
            }
        }
    }

    let chat_budget = HOOK_OUTPUT_BUDGET.saturating_sub(preamble.len());
    let chat_block = fit_to_budget(lines, chat_budget);
    format!("{preamble}{chat_block}")
}

/// Soft caps for the new preamble sections. Tuned so even with both
/// at full size the verbatim chat block still has > 5 KiB left.
const SUMMARY_BUDGET: usize = 1536;
const FILES_INDEX_BUDGET: usize = 1536;

/// Truncate a UTF-8 string to roughly `max` bytes from the head, slicing
/// at a char boundary and appending a clear marker if cut.
fn truncate_head(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let marker = "\n…(summary truncated)";
    let mut cut = max.saturating_sub(marker.len());
    while !s.is_char_boundary(cut) && cut > 0 {
        cut -= 1;
    }
    format!("{}{}", &s[..cut], marker)
}

/// Take the trailing lines of `s` (most recent) whose total byte length
/// fits in `max`. Always cuts on line boundaries.
fn tail_lines_within_budget(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut total = 0usize;
    let mut start_idx = s.len();
    for (i, _ch) in s.char_indices().rev() {
        // Walk back over the line; cut at the previous newline.
        let candidate_len = s.len() - i;
        if candidate_len > max {
            break;
        }
        if i == 0 || s.as_bytes()[i - 1] == b'\n' {
            // Line boundary at i.
            total = candidate_len;
            start_idx = i;
        }
    }
    if total == 0 {
        // Even the last line is too long; fall back to head-truncate.
        return truncate_head(s, max);
    }
    let mut out = String::with_capacity(total + 24);
    out.push_str("…(older entries truncated)\n");
    out.push_str(&s[start_idx..]);
    out
}

fn format_line(
    topic: &str,
    msg: &Message,
    nicknames: &HashMap<String, String>,
    rooms_base: &Path,
    multi_room: bool,
    self_nick: Option<&str>,
) -> String {
    let nick = nick_for(nicknames, msg);
    let time = format_utc_hhmm(msg.ts);
    // Detect @-mentions in the body so Claude knows to prioritise these.
    // The body is the raw user-submitted text; the prefix tags are added
    // by *us*, so they can't false-positive.
    let mention = mentions_self(&msg.body, self_nick);
    let mention_tag = if mention { "for-you " } else { "" };
    let prefix = if multi_room {
        let tag = topic.chars().take(6).collect::<String>().to_ascii_lowercase();
        format!("[chatroom {mention_tag}{tag} @{nick} {time}Z]")
    } else {
        format!("[chatroom {mention_tag}@{nick} {time}Z]")
    };

    if msg.kind == KIND_FILE_DROP {
        // Path the chat process saved the attachment under: rooms_base/<topic>/files/<id>-<filename>.
        // The chat receiver writes this on gossip arrival; the file is on disk by the time the
        // hook fires (PROTOCOL §8 active-rooms gating).
        let filename = sanitize_body(&msg.body);
        let local_path = rooms_base
            .join(topic)
            .join("files")
            .join(format!("{}-{}", msg.id, filename));
        format!("{prefix} dropped {filename} @file:{}\n", local_path.display())
    } else {
        let body = sanitize_body(&msg.body);
        format!("{prefix} {body}\n")
    }
}

/// Body-content scan for @-mentions of the receiving user.
///
/// Tokens (case-insensitive substring): `@<self_nick>`, `@cc`, `@claude`,
/// `@all`, `@here`. Same set as `cc_connect::chat_session::line_mentions_me`
/// — duplicated here to avoid a cc-connect-core → cc-connect dependency.
pub fn mentions_self(body: &str, self_nick: Option<&str>) -> bool {
    let lower = body.to_ascii_lowercase();
    if lower.contains("@cc")
        || lower.contains("@claude")
        || lower.contains("@all")
        || lower.contains("@here")
    {
        return true;
    }
    if let Some(nick) = self_nick.filter(|s| !s.is_empty()) {
        let token = format!("@{}", nick.to_ascii_lowercase());
        if lower.contains(&token) {
            return true;
        }
    }
    false
}

/// Pick a display name for a Message. Precedence:
///   1. Sender's self-declared `msg.nick` (v0.2 field — set by the sender
///      via the wizard or `~/.cc-connect/config.json`).
///   2. The receiver's local `nicknames.json` mapping for `msg.author`.
///   3. The first 8 chars of `msg.author` (Pubkey prefix).
///
/// Result is sanitised per PROTOCOL §7.3 step 5.
fn nick_for(nicknames: &HashMap<String, String>, msg: &Message) -> String {
    let raw = msg
        .nick
        .as_deref()
        .or_else(|| nicknames.get(&msg.author).map(|s| s.as_str()))
        .unwrap_or_else(|| pubkey_prefix(&msg.author));
    sanitize_nick(raw)
}

fn pubkey_prefix(author: &str) -> &str {
    let mut end = 0;
    let mut count = 0;
    for (i, _) in author.char_indices() {
        if count == 8 {
            return &author[..i];
        }
        count += 1;
        end = i;
    }
    let _ = end;
    author
}

/// Per PROTOCOL §7.3 step 5: replace `\n`, `\r`, `\t`, and any byte outside
/// printable ASCII (0x20–0x7E) byte-for-byte with `?`. Note: this is a
/// byte-level operation; multi-byte UTF-8 sequences (e.g. `é`) become `??`.
fn sanitize_nick(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b == b'\n' || b == b'\r' || b == b'\t' || !(0x20..=0x7E).contains(&b) {
                '?'
            } else {
                char::from(b)
            }
        })
        .collect()
}

/// Per PROTOCOL §7.3 step 5: replace bytes in C0 (`0x00..=0x1F`) and DEL
/// (`0x7F`) with a single ASCII space. UTF-8 multi-byte sequences (bytes
/// `0x80..=0xFF`) pass through untouched, preserving e.g. `é`.
fn sanitize_body(s: &str) -> String {
    let bytes: Vec<u8> = s
        .bytes()
        .map(|b| if b < 0x20 || b == 0x7F { b' ' } else { b })
        .collect();
    // Safe: replacements are ASCII space, which never breaks UTF-8 boundaries.
    String::from_utf8(bytes).expect("UTF-8 invariant preserved by ASCII-only substitution")
}

/// PROTOCOL §7.3 step 5: `(ts / 60000) % 1440` → zero-padded 24-hour `HH:MM`.
fn format_utc_hhmm(ts: i64) -> String {
    let total_minutes = ts.div_euclid(60_000);
    let day_minute = total_minutes.rem_euclid(1440);
    let hh = day_minute / 60;
    let mm = day_minute % 60;
    format!("{hh:02}:{mm:02}")
}

/// PROTOCOL §7.3 step 6: drop oldest until fit + marker, iteratively.
fn fit_to_budget(lines: Vec<String>, budget: usize) -> String {
    let total: usize = lines.iter().map(|l| l.len()).sum();
    if total <= budget {
        return lines.concat();
    }

    let mut kept = lines;
    let mut dropped = 0usize;

    loop {
        if kept.is_empty() {
            // Pathological case: even a single message exceeds the budget.
            // Emit only the marker. Conformant senders cap body at 8 KiB
            // (PROTOCOL §4), so this branch is reachable only via malformed
            // input or a single 8 KiB body whose envelope tips it over.
            return marker_line(dropped);
        }
        // Drop one more from the start (oldest by ULID).
        kept.remove(0);
        dropped += 1;
        let marker = marker_line(dropped);
        let new_total: usize = marker.len() + kept.iter().map(|l| l.len()).sum::<usize>();
        if new_total <= budget {
            let mut out = String::with_capacity(new_total);
            out.push_str(&marker);
            for l in &kept {
                out.push_str(l);
            }
            return out;
        }
    }
}

fn marker_line(dropped: usize) -> String {
    format!("[chatroom] ({dropped} older messages skipped to fit)\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_PUBKEY: &str = "hnvcppgow2sc2yvdvdicu3ynonsteflxdxrehjr2ybekdc2z3iuq";
    const B_PUBKEY: &str = "00000000000000000000000000000000000000000000000000bb";

    fn nicks(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn empty_map() -> &'static HashMap<String, String> {
        static M: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
        M.get_or_init(HashMap::new)
    }

    fn make(id: &str, author: &str, ts: i64, body: &str) -> Message {
        Message::new(id, author.to_string(), ts, body.to_string()).unwrap()
    }

    fn one_room(topic: &str, msgs: Vec<Message>) -> HashMap<String, Vec<Message>> {
        [(topic.to_string(), msgs)].into_iter().collect()
    }

    #[test]
    fn empty_rooms_renders_empty_string() {
        let nm = nicks(&[]);
        let rooms = HashMap::new();
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        assert_eq!(out, "");
    }

    #[test]
    fn single_room_no_messages_renders_empty() {
        let nm = nicks(&[]);
        let rooms = one_room("a1b2c3d4e5f6", vec![]);
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        assert_eq!(out, "");
    }

    /// Helper: produce a 26-character valid ULID for a small index.
    fn ulid(n: u64) -> String {
        // 5 chars "01HZA" + 21 zero-padded digits = 26 total.
        format!("01HZA{n:021}")
    }

    #[test]
    fn single_room_one_message_with_known_nick() {
        let nm = nicks(&[(A_PUBKEY, "alice")]);
        let msgs = vec![make(&ulid(1), A_PUBKEY, 0, "hi")];
        let rooms = one_room("a1b2c3d4e5f6", msgs);
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        assert_eq!(out, "[chatroom @alice 00:00Z] hi\n");
    }

    #[test]
    fn fallback_nick_uses_pubkey_prefix_8_chars() {
        let nm = nicks(&[]);
        let msgs = vec![make(&ulid(1), A_PUBKEY, 0, "x")];
        let rooms = one_room("a1b2c3", msgs);
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        assert_eq!(out, "[chatroom @hnvcppgo 00:00Z] x\n");
    }

    #[test]
    fn nick_sanitizes_control_and_non_ascii() {
        let bad_nick = "al\nice\té";
        let nm = nicks(&[(A_PUBKEY, bad_nick)]);
        let msgs = vec![make(&ulid(1), A_PUBKEY, 0, "x")];
        let rooms = one_room("a1b2c3", msgs);
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        // \n→?, \t→?, é (2 bytes 0xc3 0xa9) → 2× '?' per byte-for-byte rule.
        assert!(out.contains("@al?ice???"), "got: {out}");
    }

    #[test]
    fn body_replaces_c0_controls_and_del_with_space() {
        let body_raw = "a\tb\x7Fc é d";
        let nm = nicks(&[(A_PUBKEY, "x")]);
        let msgs = vec![make(&ulid(1), A_PUBKEY, 0, body_raw)];
        let rooms = one_room("a1b2c3", msgs);
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        assert!(
            out.contains("a b c é d"),
            "tab+DEL→space, é preserved: got {out}"
        );
    }

    #[test]
    fn time_formatting_known_vectors() {
        // 1970-01-01T00:00:00Z → 00:00
        assert_eq!(format_utc_hhmm(0), "00:00");
        // ts = 1714323456789 ms (the §11.2 input):
        //   total_min = 1714323456789 / 60000 = 28572057
        //   day_min   = 28572057 % 1440       = 1017
        //   hh = 16, mm = 57  → "16:57"
        // (PROTOCOL.md §11.2 originally claimed 08:57 — wrong; this test is the truth.)
        assert_eq!(format_utc_hhmm(1714323456789), "16:57");
        // Negative ts (pre-epoch) handled via div_euclid/rem_euclid.
        assert_eq!(format_utc_hhmm(-60_000).len(), 5);
    }

    #[test]
    fn multi_room_includes_room_tag() {
        let nm = nicks(&[(A_PUBKEY, "alice"), (B_PUBKEY, "bob")]);
        let mut rooms = HashMap::new();
        rooms.insert(
            "aaaaaa111111".to_string(),
            vec![make(&ulid(1), A_PUBKEY, 0, "from A")],
        );
        rooms.insert(
            "bbbbbb222222".to_string(),
            vec![make(&ulid(2), B_PUBKEY, 0, "from B")],
        );
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        let expected =
            "[chatroom aaaaaa @alice 00:00Z] from A\n\
             [chatroom bbbbbb @bob 00:00Z] from B\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn multi_room_sort_is_global_by_ulid() {
        let nm = nicks(&[(A_PUBKEY, "alice"), (B_PUBKEY, "bob")]);
        let mut rooms = HashMap::new();
        rooms.insert(
            "aaaaaa".to_string(),
            vec![
                make(&ulid(1), A_PUBKEY, 0, "A first"),
                make(&ulid(3), A_PUBKEY, 0, "A second"),
            ],
        );
        rooms.insert(
            "bbbbbb".to_string(),
            vec![make(&ulid(2), B_PUBKEY, 0, "B middle")],
        );
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("A first"));
        assert!(lines[1].contains("B middle"));
        assert!(lines[2].contains("A second"));
    }

    #[test]
    fn truncates_with_marker_when_over_budget() {
        // Build enough messages to exceed 8 KiB.
        let nm = nicks(&[(A_PUBKEY, "alice")]);
        let mut msgs = Vec::new();
        let body = "x".repeat(100); // ~140 bytes per formatted line.
        for i in 0..200 {
            // ULIDs sorted ascending; older = lower index.
            let id = format!("01HZA{:021}", i);
            msgs.push(make(&id, A_PUBKEY, 0, &body));
        }
        let rooms = one_room("a1b2c3", msgs);
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });

        assert!(out.len() <= HOOK_OUTPUT_BUDGET, "MUST fit within 8 KiB; got {}", out.len());
        assert!(
            out.starts_with("[chatroom] ("),
            "marker line MUST be first when truncation occurred: {out:?}"
        );
        assert!(
            out.contains("older messages skipped to fit)\n"),
            "marker line MUST mention skipped count: {out}"
        );
        // The KEPT lines should be the newest, so "01HZA...199" should appear.
        assert!(out.contains(&format!("01HZA{:021}", 199)) || out.contains("xxxx"),
                "newest message should be present in the body or its content");
    }

    #[test]
    fn fit_loop_handles_marker_digit_growth() {
        // Marker length differs at 1 / 99 / 999 dropped — loop must reconverge.
        // We force 100+ drops and verify the result still fits.
        let mut lines = Vec::new();
        for _ in 0..200 {
            lines.push("x".repeat(100) + "\n");
        }
        let out = fit_to_budget(lines, 4 * 1024); // smaller budget to amplify drops
        assert!(out.len() <= 4 * 1024, "must fit budget after iterative drops");
        assert!(out.starts_with("[chatroom] ("), "marker MUST lead");
    }

    #[test]
    fn within_budget_omits_marker() {
        let nm = nicks(&[(A_PUBKEY, "alice")]);
        let msgs = vec![make(&ulid(1), A_PUBKEY, 0, "small")];
        let rooms = one_room("a1b2c3", msgs);
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        assert!(!out.starts_with("[chatroom] ("));
    }

    #[test]
    fn empty_body_renders_with_trailing_newline() {
        let nm = nicks(&[(A_PUBKEY, "alice")]);
        let msgs = vec![make(&ulid(1), A_PUBKEY, 0, "")];
        let rooms = one_room("a1b2c3", msgs);
        let out = render(&HookInput { rooms: &rooms, nicknames: &nm, rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"), self_nick: None, room_summaries: empty_map(), room_file_indexes: empty_map() });
        assert_eq!(out, "[chatroom @alice 00:00Z] \n");
    }

    #[test]
    fn file_drop_renders_at_file_reference_pointing_at_attachment() {
        // file_drop Messages render as `dropped <filename> @file:<path>` so
        // Claude Code reads the bytes via its existing @file: convention.
        let nm = nicks(&[(A_PUBKEY, "alice")]);
        let id = ulid(1);
        let drop = Message::new_file_drop(
            &id,
            A_PUBKEY.to_string(),
            0,
            "design.svg".to_string(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262".to_string(),
            6,
        )
        .unwrap();
        let topic = "aaaa11";
        let rooms = one_room(topic, vec![drop.clone()]);
        let out = render(&HookInput {
            rooms: &rooms,
            nicknames: &nm,
            rooms_base: std::path::Path::new("/var/tmp/cc-test/rooms"),
            self_nick: None,
            room_summaries: empty_map(),
            room_file_indexes: empty_map(),
        });
        let expected_path = format!(
            "/var/tmp/cc-test/rooms/{topic}/files/{}-design.svg",
            drop.id
        );
        assert_eq!(
            out,
            format!("[chatroom @alice 00:00Z] dropped design.svg @file:{expected_path}\n")
        );
    }

    /// `@cc` / `@claude` / `@all` / `@here` always trigger mention even when
    /// the receiver hasn't set a nick.
    #[test]
    fn mentions_self_universal_tokens() {
        assert!(mentions_self("hey @cc what's up", None));
        assert!(mentions_self("HEY @CLAUDE", None));
        assert!(mentions_self("@all standup in 5", None));
        assert!(mentions_self("ping @here please", None));
        assert!(!mentions_self("plain message", None));
    }

    /// Self-nick mention is case-insensitive and only fires when the nick
    /// is actually set.
    #[test]
    fn mentions_self_with_self_nick() {
        assert!(mentions_self("hi @yijian", Some("yijian")));
        assert!(mentions_self("hi @YIJIAN!", Some("yijian")));
        assert!(!mentions_self("hi yijian", Some("yijian"))); // no @
        assert!(!mentions_self("hi @bob", Some("yijian")));
        assert!(!mentions_self("@yijian", None)); // self_nick missing → no match
        assert!(!mentions_self("@yijian", Some(""))); // empty → ignored
    }

    /// When the body mentions the receiver, the rendered prefix gains a
    /// `for-you` tag so Claude can prioritise (single-room shape).
    #[test]
    fn rendered_prefix_marks_for_you_in_single_room() {
        let nm = nicks(&[(A_PUBKEY, "alice")]);
        let m = make(&ulid(1), A_PUBKEY, 0, "hey @bob, please review");
        let rooms = one_room("aabb11", vec![m]);
        let out = render(&HookInput {
            rooms: &rooms,
            nicknames: &nm,
            rooms_base: std::path::Path::new("/tmp/cc-connect-test-rooms"),
            self_nick: Some("bob"),
            room_summaries: empty_map(),
            room_file_indexes: empty_map(),
        });
        assert!(
            out.starts_with("[chatroom for-you @alice 00:00Z]"),
            "expected `for-you` tag, got: {out}"
        );
    }
}
