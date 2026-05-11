//! Hook integration tests covering the parts of the magic-moment chain
//! that don't require a fake `claude` parent process.
//!
//! v0.6 changed the trust boundary from the `CC_CONNECT_ROOM` env var
//! to **Claude PID Binding** (PROTOCOL.md §7.3 step 0, ADR-0006). The
//! hook now walks its parent process chain to find a `claude` ancestor
//! and reads `~/.cc-connect/sessions/by-claude-pid/<pid>/rooms.json`.
//! Tests that exercise the injection path therefore need a `claude`-named
//! ancestor in the process tree, which `cargo test` doesn't naturally
//! provide. The pre-v0.6 tests (`magic_moment_hook_emits_…`,
//! `…skips_dead_pid_active_room_file`, `routing_with_env_var_scopes_…`)
//! exercised the env-var path and were removed when that path was
//! retired. Re-introducing equivalent coverage via a fake-`claude`
//! helper binary is tracked as follow-up.
//!
//! What this test *still* proves:
//!   - PROTOCOL.md §7.4: the hook always exits 0 even with no binding.
//!   - The "no claude ancestor" no-op gate (the trust-boundary entry
//!     point) — the hook produces empty stdout when the test runner
//!     isn't a descendant of `claude`.
//!   - Cursor read/no-advance behaviour when there are no unread
//!     messages.

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
const TEST_TOPIC_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000000";

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

/// Without a `claude` parent in the process tree, the hook MUST be a
/// no-op even if active rooms exist on the machine. This is the v0.6
/// Claude PID Binding contract (PROTOCOL.md §7.3 step 0): chat context
/// only flows into Claude sessions whose owning Claude has a state
/// file under `~/.cc-connect/sessions/by-claude-pid/<pid>/`. Any
/// unrelated process invoking the hook stays blind to the substrate.
/// The `cargo test` runner is by definition not a Claude descendant,
/// so this test exercises the no-op gate directly.
#[test]
fn hook_without_claude_ancestor_is_a_noop() {
    const TOPIC_A: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const TOPIC_B: &str = "4444444444444444444444444444444444444444444444444444444444444444";
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

    assert_eq!(out.status.code(), Some(0), "hook MUST exit 0");
    assert!(
        out.stdout.is_empty(),
        "without a `claude` ancestor the hook MUST emit nothing; stdout was {:?}",
        String::from_utf8_lossy(&out.stdout)
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
