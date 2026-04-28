//! Internal POSIX helpers shared across cc-connect-core I/O modules.
//!
//! This is `pub(crate)` only — public callers should not depend on these
//! exact entry points; they're plumbing for `log_io` and `cursor_io`.
//!
//! See `PROTOCOL.md` §5 (writer locks), §7.3 step 8 (cursor lock + race),
//! and §7.4 (lock unification rationale).

use anyhow::{anyhow, Result};
use std::fs::File;
use std::os::fd::AsFd;

/// Mode for the kind of advisory lock we want to take on a file.
#[derive(Copy, Clone)]
pub(crate) enum LockKind {
    /// `LOCK_SH` equivalent — multiple readers, blocks writers.
    Shared,
    /// `LOCK_EX` equivalent — exclusive single holder, blocks everyone else.
    Exclusive,
}

/// Acquire a blocking advisory lock on `file`.
///
/// Uses `rustix::fs::fcntl_lock`. On Linux this calls `fcntl(F_OFD_SETLKW)`
/// (per-fd, the modern preferred form). On macOS the fallback may be
/// `fcntl(F_SETLKW)` (per-process), so callers within the same process
/// that share a file via separate `File` handles cannot rely on this for
/// serialisation — see `log_io::append`'s single-syscall write strategy
/// for the actual cross-thread atomicity guarantee. Cross-*process*
/// serialisation works on both kernels.
pub(crate) fn acquire_lock(file: &File, kind: LockKind) -> Result<()> {
    use rustix::fs::{fcntl_lock, FlockOperation};
    let op = match kind {
        LockKind::Shared => FlockOperation::LockShared,
        LockKind::Exclusive => FlockOperation::LockExclusive,
    };
    fcntl_lock(file.as_fd(), op).map_err(|e| anyhow!("fcntl lock acquire: {e}"))
}

/// Release any lock held on `file`. Idempotent: unlocking an already-unlocked
/// file is a no-op.
pub(crate) fn release_lock(file: &File) -> Result<()> {
    use rustix::fs::{fcntl_lock, FlockOperation};
    fcntl_lock(file.as_fd(), FlockOperation::Unlock)
        .map_err(|e| anyhow!("fcntl lock release: {e}"))
}
