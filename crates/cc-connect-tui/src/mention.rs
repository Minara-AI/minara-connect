//! @-mention completion: figure out the in-progress `@<prefix>` token in
//! the chat input box and produce a candidate list from recently-seen
//! peer nicks plus the special broadcast tokens (`cc`, `claude`, `all`,
//! `here`).
//!
//! Pure functions; the popup state machine lives in `event_loop.rs`.

use std::collections::VecDeque;

/// Special broadcast tokens. Hook (`hook_format::mentions_self`) treats
/// these as "everyone listening" — keep this list in sync with
/// `cc-connect-core/src/hook_format.rs::mentions_self`.
const BROADCAST_TOKENS: &[&str] = &["cc", "claude", "all", "here"];

/// If `input` ends with an in-progress `@<prefix>` (no space after the
/// last `@`), return the prefix (without the `@`). The prefix may be
/// empty when the user has just typed `@`.
pub fn current_at_token(input: &str) -> Option<&str> {
    // Find the last `@`. If there's whitespace between it and the end,
    // the token is finished and we don't suggest.
    let at = input.rfind('@')?;
    let after = &input[at + 1..];
    if after.chars().any(char::is_whitespace) {
        return None;
    }
    // Optional: avoid triggering on email-like patterns ("foo@bar"). If
    // the char immediately before `@` is alphanumeric, treat it as not a
    // mention.
    if at > 0 {
        let prev = input[..at].chars().next_back().unwrap();
        if prev.is_alphanumeric() {
            return None;
        }
    }
    Some(after)
}

/// Filter `recent` (most-recent-first) by case-insensitive `starts_with`,
/// excluding `self_nick` and the user's `<self_nick>-cc` alias (you don't
/// @-mention yourself), then append broadcast tokens that match.
pub fn mention_candidates<'a>(
    recent: &'a VecDeque<String>,
    prefix: &str,
    self_nick: Option<&str>,
) -> Vec<String> {
    let lower = prefix.to_ascii_lowercase();
    let self_lower = self_nick.map(|s| s.to_ascii_lowercase());
    let self_cc = self_nick.map(|s| format!("{}-cc", s.to_ascii_lowercase()));

    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for nick in recent.iter() {
        let n_lower = nick.to_ascii_lowercase();
        if Some(&n_lower) == self_lower.as_ref() {
            continue;
        }
        if Some(&n_lower) == self_cc.as_ref() {
            continue;
        }
        if !n_lower.starts_with(&lower) {
            continue;
        }
        if seen.insert(n_lower) {
            out.push(nick.clone());
        }
    }

    for tok in BROADCAST_TOKENS {
        if !tok.starts_with(&lower) {
            continue;
        }
        let lower_tok = tok.to_ascii_lowercase();
        if seen.insert(lower_tok) {
            out.push((*tok).to_string());
        }
    }
    out
}

/// Replace the trailing `@<prefix>` in `input` with `@<full> ` (with a
/// trailing space so the user can keep typing the body).
pub fn complete_at(input: &mut String, full_nick: &str) {
    if let Some(at) = input.rfind('@') {
        input.truncate(at + 1);
        input.push_str(full_nick);
        input.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nicks(items: &[&str]) -> VecDeque<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn at_token_at_end() {
        assert_eq!(current_at_token(""), None);
        assert_eq!(current_at_token("hello"), None);
        assert_eq!(current_at_token("hello @"), Some(""));
        assert_eq!(current_at_token("hello @ali"), Some("ali"));
        assert_eq!(current_at_token("@bob"), Some("bob"));
    }

    #[test]
    fn at_token_finished_by_space() {
        assert_eq!(current_at_token("@alice "), None);
        assert_eq!(current_at_token("@alice hi"), None);
    }

    #[test]
    fn no_email_match() {
        assert_eq!(current_at_token("foo@bar"), None);
    }

    #[test]
    fn candidates_filter_and_dedupe() {
        // Recent contains a duplicate (case-insensitive); broadcast token
        // "all" also matches "al". Both should appear, no dups.
        let recent = nicks(&["Alice", "BOB", "alice"]);
        let got = mention_candidates(&recent, "al", None);
        assert_eq!(got, vec!["Alice".to_string(), "all".into()]);
    }

    #[test]
    fn candidates_skip_self_and_self_cc() {
        let recent = nicks(&["YJ", "YJ-cc", "alice"]);
        let got = mention_candidates(&recent, "", Some("YJ"));
        assert_eq!(got, vec!["alice".to_string(), "cc".into(), "claude".into(), "all".into(), "here".into()]);
    }

    #[test]
    fn broadcast_tokens_appended() {
        let recent = nicks(&[]);
        let got = mention_candidates(&recent, "c", None);
        assert_eq!(got, vec!["cc".to_string(), "claude".into()]);
    }

    #[test]
    fn complete_at_replaces_partial() {
        let mut s = "hello @al".to_string();
        complete_at(&mut s, "alice");
        assert_eq!(s, "hello @alice ");
    }
}
