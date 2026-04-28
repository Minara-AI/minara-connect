//! v0.1 release-gate integration test (PROTOCOL.md §11.4).
//!
//! Exercises the **magic-moment core** — the chain that takes a chat
//! Message in a Room and surfaces it in Claude Code's prompt context —
//! end-to-end, with the actual `cc-connect-hook` binary as a subprocess.
//!
//! Skipped from the loop:
//!   - iroh transport (chat → gossip → log on the receiving peer):
//!     stubbed out by writing the receiver's log.jsonl directly.
//!     iroh integration is verified by the manual two-laptop smoke
//!     test in the README. Adding a real two-peer iroh test here would
//!     introduce network dependencies (relay, NAT) that don't belong
//!     in `cargo test`.
//!
//! What this test *does* prove (the v0.1 release contract):
//!   - PROTOCOL.md §7.3 steps 1-9 (hook flow) end-to-end.
//!   - PROTOCOL.md §8 active-rooms PID semantics + sweep.
//!   - PROTOCOL.md §9 cursor advance is atomic and lands at the right id.
//!   - PROTOCOL.md §7.3 step 5 single-Room format string is byte-exact.

use cc_connect_core::{log_io, message::Message};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const TEST_PUBKEY: &str = "hnvcppgow2sc2yvdvdicu3ynonsteflxdxrehjr2ybekdc2z3iuq";
const TEST_MSG_ID: &str = "01HZA8K9F0RS3JXG7QZ4N5VTBC"; // 26 chars, §11.1 ref pubkey + arbitrary id
const TEST_TS_MS: i64 = 0; // 1970-01-01T00:00:00Z → "00:00Z" in HH:MMZ
const TEST_BODY: &str = "hello from the magic moment";
const TEST_SESSION: &str = "test-session-001";
// 64 lowercase hex chars = a 32-byte topic id of all zeros.
const TEST_TOPIC_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

#[test]
fn magic_moment_hook_emits_canonical_chatroom_line_and_advances_cursor() {
    let env = TestEnv::setup();
    env.seed_active_room_with_unread_message();

    // Spawn cc-connect-hook with a UserPromptSubmit-style stdin payload.
    let stdin_payload = format!(r#"{{"session_id":"{TEST_SESSION}"}}"#);
    let mut child = Command::new(&env.hook_bin)
        .env_clear()
        .env("HOME", &env.home)
        .env("TMPDIR", &env.tmpdir)
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| String::new()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cc-connect-hook");

    child
        .stdin
        .as_mut()
        .expect("hook stdin")
        .write_all(stdin_payload.as_bytes())
        .expect("write hook stdin");

    let out = child.wait_with_output().expect("wait hook");

    // PROTOCOL.md §7.4: hook MUST always exit 0.
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook MUST exit 0 (got {:?}); stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    // PROTOCOL.md §7.3 step 5 single-Room format:
    //   `[chatroom @<nick> <hh:mm>Z] <body>\n`
    // Without nicknames.json, nick falls back to the first 8 chars of the Pubkey.
    let expected = format!(
        "[chatroom @{} 00:00Z] {}\n",
        &TEST_PUBKEY[..8],
        TEST_BODY
    );
    let actual = String::from_utf8(out.stdout).expect("hook stdout is UTF-8");
    assert_eq!(
        actual, expected,
        "hook stdout must match the canonical single-Room format byte-exact"
    );

    // PROTOCOL.md §9 + §7.3 step 8: cursor advanced atomically to the Message id.
    let cursor_path = env
        .home
        .join(".cc-connect")
        .join("cursors")
        .join(TEST_TOPIC_HEX)
        .join(format!("{TEST_SESSION}.cursor"));
    let raw = std::fs::read_to_string(&cursor_path)
        .expect("cursor file must exist after hook fires");
    assert_eq!(
        raw.trim_end_matches('\n'),
        TEST_MSG_ID,
        "cursor MUST contain the Message id we just injected"
    );
}

#[test]
fn magic_moment_hook_emits_nothing_when_cursor_already_at_tail() {
    let env = TestEnv::setup();
    env.seed_active_room_with_unread_message();

    // Pre-seed the cursor at the message id; nothing should be unread.
    let cursor_dir = env
        .home
        .join(".cc-connect")
        .join("cursors")
        .join(TEST_TOPIC_HEX);
    std::fs::create_dir_all(&cursor_dir).unwrap();
    std::fs::set_permissions(&cursor_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cursor_path = cursor_dir.join(format!("{TEST_SESSION}.cursor"));
    std::fs::write(&cursor_path, TEST_MSG_ID).unwrap();

    let out = Command::new(&env.hook_bin)
        .env_clear()
        .env("HOME", &env.home)
        .env("TMPDIR", &env.tmpdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin
                .as_mut()
                .unwrap()
                .write_all(format!(r#"{{"session_id":"{TEST_SESSION}"}}"#).as_bytes())?;
            c.wait_with_output()
        })
        .expect("hook child");

    assert_eq!(out.status.code(), Some(0), "hook MUST exit 0");
    assert!(
        out.stdout.is_empty(),
        "no unread Messages → empty stdout (PROTOCOL §7.3 step 9), got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn magic_moment_hook_skips_dead_pid_active_room_file() {
    let env = TestEnv::setup_no_room();
    // Drop a stale PID file from a long-dead PID (use 99999 — this far
    // exceeds anything plausibly running, and falls in the valid-PID
    // range we accept).
    let active_dir = env.active_rooms_dir();
    std::fs::create_dir_all(&active_dir).unwrap();
    std::fs::set_permissions(&active_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let stale = active_dir.join(format!("{TEST_TOPIC_HEX}.active"));
    std::fs::write(&stale, "99999").unwrap();

    let out = Command::new(&env.hook_bin)
        .env_clear()
        .env("HOME", &env.home)
        .env("TMPDIR", &env.tmpdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin
                .as_mut()
                .unwrap()
                .write_all(format!(r#"{{"session_id":"{TEST_SESSION}"}}"#).as_bytes())?;
            c.wait_with_output()
        })
        .expect("hook child");

    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "dead-PID room MUST NOT inject");
    // The stale PID file should also be swept by the hook.
    assert!(
        !stale.exists(),
        "stale .active file MUST be unlinked by the hook (PROTOCOL §8)"
    );
}

/// CC_CONNECT_ROOM env routing: with two active rooms planted, the hook
/// must inject ONLY the room whose topic matches the env var, and use the
/// single-room prefix shape (no `<topic_short>` tag).
#[test]
fn routing_with_env_var_scopes_to_one_room() {
    const TOPIC_A: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";
    const TOPIC_B: &str =
        "2222222222222222222222222222222222222222222222222222222222222222";
    const ID_A: &str = "01HZA8K9F0RS3JXG7QZ4N5VTBA";
    const ID_B: &str = "01HZA8K9F0RS3JXG7QZ4N5VTBB";
    const BODY_A: &str = "from-room-A";
    const BODY_B: &str = "from-room-B";

    let env = TestEnv::setup_no_room();
    env.seed_active_room(TOPIC_A, ID_A, BODY_A);
    env.seed_active_room(TOPIC_B, ID_B, BODY_B);

    let stdin_payload = format!(r#"{{"session_id":"{TEST_SESSION}"}}"#);
    let out = Command::new(&env.hook_bin)
        .env_clear()
        .env("HOME", &env.home)
        .env("TMPDIR", &env.tmpdir)
        .env("CC_CONNECT_ROOM", TOPIC_A)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin
                .as_mut()
                .unwrap()
                .write_all(stdin_payload.as_bytes())?;
            c.wait_with_output()
        })
        .expect("hook child");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let expected = format!(
        "[chatroom @{} 00:00Z] {}\n",
        &TEST_PUBKEY[..8],
        BODY_A
    );
    assert_eq!(
        stdout, expected,
        "CC_CONNECT_ROOM MUST scope to exactly one room and use the single-room prefix"
    );
    assert!(
        !stdout.contains(BODY_B),
        "non-matching room MUST NOT appear in output"
    );
}

/// Without `CC_CONNECT_ROOM` set, the hook keeps the legacy "inject all
/// active rooms" behaviour for back-compat with standalone Claude Code.
#[test]
fn routing_without_env_var_includes_all_rooms() {
    const TOPIC_A: &str =
        "3333333333333333333333333333333333333333333333333333333333333333";
    const TOPIC_B: &str =
        "4444444444444444444444444444444444444444444444444444444444444444";
    const ID_A: &str = "01HZA8K9F0RS3JXG7QZ4N5VTAA";
    const ID_B: &str = "01HZA8K9F0RS3JXG7QZ4N5VTAB";
    const BODY_A: &str = "alpha-room-msg";
    const BODY_B: &str = "beta-room-msg";

    let env = TestEnv::setup_no_room();
    env.seed_active_room(TOPIC_A, ID_A, BODY_A);
    env.seed_active_room(TOPIC_B, ID_B, BODY_B);

    let stdin_payload = format!(r#"{{"session_id":"{TEST_SESSION}"}}"#);
    let out = Command::new(&env.hook_bin)
        .env_clear()
        .env("HOME", &env.home)
        .env("TMPDIR", &env.tmpdir)
        // No CC_CONNECT_ROOM — legacy mode.
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin
                .as_mut()
                .unwrap()
                .write_all(stdin_payload.as_bytes())?;
            c.wait_with_output()
        })
        .expect("hook child");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        stdout.contains(BODY_A),
        "legacy mode MUST include room A; stdout was {stdout:?}"
    );
    assert!(
        stdout.contains(BODY_B),
        "legacy mode MUST include room B; stdout was {stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// Test fixture: tempdir-backed HOME + TMPDIR plus a built cc-connect-hook bin.
// ---------------------------------------------------------------------------

struct TestEnv {
    _home_guard: tempfile::TempDir,
    _tmp_guard: tempfile::TempDir,
    home: PathBuf,
    tmpdir: PathBuf,
    hook_bin: PathBuf,
}

impl TestEnv {
    fn setup_no_room() -> Self {
        // Locate the cc-connect-hook binary in the same target profile dir
        // as our test runner. `current_exe` returns the test's own executable,
        // e.g. .../target/debug/deps/integration_xxx-HASH; walk up to the
        // build profile root and look for `cc-connect-hook`.
        //
        // Pre-condition: the user invoked `cargo test --workspace` (or built
        // cc-connect-hook explicitly first). Trying to invoke `cargo build`
        // from inside `cargo test` deadlocks on the build directory lock.
        let test_exe = std::env::current_exe().expect("current_exe");
        let target_dir = test_exe
            .parent()
            .expect("no deps dir parent")
            .parent()
            .expect("no profile dir parent")
            .to_path_buf();
        let hook_bin = target_dir.join("cc-connect-hook");
        assert!(
            hook_bin.exists(),
            "expected cc-connect-hook at {} — did you run `cargo build --workspace` (or `cargo test --workspace`) first?",
            hook_bin.display()
        );

        let home_guard = tempfile::tempdir().expect("home tempdir");
        let tmp_guard = tempfile::tempdir().expect("tmp tempdir");
        let home = home_guard.path().to_path_buf();
        let tmpdir = tmp_guard.path().to_path_buf();

        // Always-needed: ~/.cc-connect with mode 0700.
        let cc_dir = home.join(".cc-connect");
        std::fs::create_dir_all(&cc_dir).unwrap();
        std::fs::set_permissions(&cc_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        TestEnv {
            _home_guard: home_guard,
            _tmp_guard: tmp_guard,
            home,
            tmpdir,
            hook_bin,
        }
    }

    fn setup() -> Self {
        Self::setup_no_room()
    }

    fn active_rooms_dir(&self) -> PathBuf {
        let uid = rustix::process::geteuid().as_raw();
        self.tmpdir
            .join(format!("cc-connect-{uid}"))
            .join("active-rooms")
    }

    /// Plant: an active-rooms PID file pointing at this test process (so it's
    /// detected as alive); a log.jsonl with one Message; no cursor yet.
    fn seed_active_room_with_unread_message(&self) {
        self.seed_active_room(TEST_TOPIC_HEX, TEST_MSG_ID, TEST_BODY);
    }

    /// Same as [`seed_active_room_with_unread_message`] but with caller-
    /// supplied topic / message id / body, used for the multi-room routing
    /// tests.
    fn seed_active_room(&self, topic_hex: &str, msg_id: &str, body: &str) {
        let log_path = self
            .home
            .join(".cc-connect")
            .join("rooms")
            .join(topic_hex)
            .join("log.jsonl");
        let mut log_file = log_io::open_or_create_log(&log_path).expect("open log");
        let msg = Message::new(
            msg_id,
            TEST_PUBKEY.to_string(),
            TEST_TS_MS,
            body.to_string(),
        )
        .expect("valid Message");
        log_io::append(&mut log_file, &msg).expect("append");

        let active_dir = self.active_rooms_dir();
        std::fs::create_dir_all(&active_dir).unwrap();
        std::fs::set_permissions(&active_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pid_path = active_dir.join(format!("{topic_hex}.active"));
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();
    }
}
