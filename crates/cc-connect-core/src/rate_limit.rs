//! Per-author sliding-window rate limiter for incoming gossip Messages.
//!
//! Receivers consult [`RateLimiter::check_and_record`] before persisting an
//! arrival. Authors that exceed [`RATE_LIMIT_MAX_PER_WINDOW`] inside any
//! [`RATE_LIMIT_WINDOW_MS`] window are dropped on the receiver side; the
//! offender's own log persistence (on their machine) is unaffected. The
//! limiter is purely defensive and per-receiver — different peers may
//! disagree about which messages they kept.
//!
//! The `warn` flag returned on a drop is cooldown-gated by
//! [`RATE_LIMIT_WARN_COOLDOWN_MS`] so a flooder doesn't simultaneously
//! flood the receiver's own UI with rate-limit notices.

use std::collections::{HashMap, VecDeque};

/// Sliding window length, in Unix milliseconds.
pub const RATE_LIMIT_WINDOW_MS: i64 = 10_000;

/// Maximum messages from one author allowed inside the window before
/// receivers begin dropping. 30 / 10 s ≈ 3 msg/s sustained — high enough to
/// cover bursty `cc_send` chatter from a Claude, low enough that a flooder
/// is contained.
pub const RATE_LIMIT_MAX_PER_WINDOW: usize = 30;

/// Minimum gap between user-visible "rate-limited" warnings per author.
pub const RATE_LIMIT_WARN_COOLDOWN_MS: i64 = 30_000;

/// Outcome of [`RateLimiter::check_and_record`].
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RateLimitDecision {
    /// Within budget. The timestamp has been recorded; caller proceeds.
    Allow,
    /// Over budget. Caller drops the Message. `warn` is `true` only on the
    /// first drop within the cooldown — surface a one-line UI warning,
    /// silently drop subsequent overages.
    Drop { warn: bool },
}

#[derive(Default)]
pub struct RateLimiter {
    by_author: HashMap<String, VecDeque<i64>>,
    last_warned: HashMap<String, i64>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide whether `author` is over budget at `now_ms`. On `Allow` the
    /// timestamp is recorded; on `Drop` it is not (so the offender's
    /// excess doesn't keep the window topped up).
    pub fn check_and_record(&mut self, author: &str, now_ms: i64) -> RateLimitDecision {
        let window = self.by_author.entry(author.to_string()).or_default();
        while window.front().is_some_and(|t| now_ms - *t > RATE_LIMIT_WINDOW_MS) {
            window.pop_front();
        }
        if window.len() >= RATE_LIMIT_MAX_PER_WINDOW {
            let last = self.last_warned.get(author).copied().unwrap_or(i64::MIN);
            let warn = now_ms.saturating_sub(last) >= RATE_LIMIT_WARN_COOLDOWN_MS;
            if warn {
                self.last_warned.insert(author.to_string(), now_ms);
            }
            return RateLimitDecision::Drop { warn };
        }
        window.push_back(now_ms);
        RateLimitDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_cap() {
        let mut rl = RateLimiter::new();
        for i in 0..RATE_LIMIT_MAX_PER_WINDOW {
            assert_eq!(
                rl.check_and_record("alice", i as i64 * 10),
                RateLimitDecision::Allow,
                "msg {i} should be allowed"
            );
        }
    }

    #[test]
    fn drops_over_cap_with_warn_then_silently() {
        let mut rl = RateLimiter::new();
        for i in 0..RATE_LIMIT_MAX_PER_WINDOW {
            rl.check_and_record("alice", i as i64);
        }
        // First overage warns.
        assert_eq!(
            rl.check_and_record("alice", RATE_LIMIT_MAX_PER_WINDOW as i64),
            RateLimitDecision::Drop { warn: true }
        );
        // Subsequent overages within cooldown do not.
        for i in 1..5 {
            assert_eq!(
                rl.check_and_record("alice", RATE_LIMIT_MAX_PER_WINDOW as i64 + i),
                RateLimitDecision::Drop { warn: false }
            );
        }
    }

    #[test]
    fn warn_re_arms_after_cooldown() {
        let mut rl = RateLimiter::new();
        for i in 0..RATE_LIMIT_MAX_PER_WINDOW {
            rl.check_and_record("alice", i as i64);
        }
        // First overage warns.
        let t1 = RATE_LIMIT_MAX_PER_WINDOW as i64;
        assert_eq!(
            rl.check_and_record("alice", t1),
            RateLimitDecision::Drop { warn: true }
        );
        // After cooldown a fresh overage warns again.
        let t2 = t1 + RATE_LIMIT_WARN_COOLDOWN_MS + RATE_LIMIT_WINDOW_MS + 1;
        // Window has long since cleared; refill it past the cap.
        for i in 0..RATE_LIMIT_MAX_PER_WINDOW {
            rl.check_and_record("alice", t2 + i as i64);
        }
        assert_eq!(
            rl.check_and_record("alice", t2 + RATE_LIMIT_MAX_PER_WINDOW as i64),
            RateLimitDecision::Drop { warn: true }
        );
    }

    #[test]
    fn allows_again_after_window_slides() {
        let mut rl = RateLimiter::new();
        for i in 0..RATE_LIMIT_MAX_PER_WINDOW {
            rl.check_and_record("alice", i as i64);
        }
        // Slide past the window — the old timestamps fall out.
        let later = RATE_LIMIT_WINDOW_MS + 1;
        assert_eq!(
            rl.check_and_record("alice", later),
            RateLimitDecision::Allow
        );
    }

    #[test]
    fn authors_are_independent() {
        let mut rl = RateLimiter::new();
        for i in 0..RATE_LIMIT_MAX_PER_WINDOW {
            rl.check_and_record("alice", i as i64);
        }
        // Bob is fresh — gets a full window despite Alice being capped.
        assert_eq!(rl.check_and_record("bob", 0), RateLimitDecision::Allow);
        // And Alice is still capped.
        assert!(matches!(
            rl.check_and_record("alice", RATE_LIMIT_MAX_PER_WINDOW as i64),
            RateLimitDecision::Drop { .. }
        ));
    }
}
