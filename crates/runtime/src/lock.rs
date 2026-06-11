//! The cross-process, machine-local advisory write-lock (RUNTIME-LOCK).
//!
//! The real contention is a **cron `summary` process vs. an open interactive TUI
//! process** — two *processes* an in-process mutex cannot see. So the lock is a
//! machine-local **lockfile** under the cache dir (Deferred-1 in
//! `runtime-design.md` resolved to lockfile-over-Sheets-cell), holding the
//! holder's identity and an acquisition timestamp.
//!
//! It exposes a **non-blocking** [`AdvisoryLock::try_acquire`] returning
//! [`LockOutcome::Acquired`] (a held [`LockHandle`] whose `Drop` releases) or
//! [`LockOutcome::Held`]`{ holder, since }`, plus a **TTL**: a lock whose file is
//! older than the TTL is *stale* (its holder presumably crashed) and is
//! reclaimable, so a crashed TUI cannot wedge the cron summary forever
//! (RUNTIME-LOCK-001).
//!
//! It also adapts to the shared [`pt_core::Lock`] trait via [`StoreLockAdapter`],
//! so the SAME advisory lock is acquired **inside every workbook-write primitive**
//! (event append, History append, view republish, config write, import commit) —
//! coverage independent of which caller initiates the write (RUNTIME-LOCK-002).
//! When the lock is `Held`, the rich outcome is returned to the writer, which
//! applies its own policy (entry fails non-destructively, summary runs read-only,
//! import holds the whole commit, reports.append_snapshot acquires like any
//! write) (RUNTIME-LOCK-003).
//!
//! A `clock` injection keeps the TTL/stale logic deterministically testable with
//! no real sleeps; the real path uses the wall clock.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use pt_core::{Lock, LockError, LockGuard};

/// The fixed lockfile name under the cache dir. One machine-local advisory write-
/// lock guards every workbook write. (RUNTIME-LOCK-001)
pub const LOCKFILE_NAME: &str = "portfolio-tracker.write.lock";

/// The default TTL: a lockfile older than this is treated as stale (a crashed
/// holder) and reclaimable. Generous enough that a live holder's normal critical
/// section never trips it, short enough that a crash does not wedge cron forever.
/// (RUNTIME-LOCK-001, Deferred-1)
pub const DEFAULT_TTL_SECS: u64 = 300;

/// Who holds the lock: a process-identifying string written into the lockfile so
/// a `Held` outcome can name the holder. (RUNTIME-LOCK-001/003)
pub type Holder = String;

/// The non-blocking acquire result. Either the lock was acquired (a [`LockHandle`]
/// whose `Drop` releases it), or it is currently `Held` by another holder — with
/// that holder's identity and the epoch-seconds it has been held *since*, so the
/// writer can apply its policy. (RUNTIME-LOCK-001/003)
#[derive(Debug)]
pub enum LockOutcome {
    /// Acquired: the caller now owns the advisory lock until the handle drops.
    Acquired(LockHandle),
    /// Held by another holder. Carries the holder identity and the acquisition
    /// epoch-seconds. The caller applies its held-lock policy. (RUNTIME-LOCK-003)
    Held {
        /// The current holder's identity (as recorded in the lockfile).
        holder: Holder,
        /// Epoch-seconds when the current holder acquired the lock.
        since: u64,
    },
    /// The acquire itself failed for an I/O reason OTHER than contention — the
    /// lock path is unwritable (the parent dir cannot be created, or the
    /// exclusive create fails with anything other than `AlreadyExists`). This is
    /// surfaced as an acquisition error to the caller rather than silently
    /// proceeding as if `Acquired` OR falsely claiming the lock is `Held` by
    /// another holder (which would mislead a held-lock policy). (RUNTIME-LOCK-007)
    Error {
        /// A short human-readable reason for the acquisition failure.
        reason: String,
    },
}

impl LockOutcome {
    /// Whether the lock was acquired.
    pub fn is_acquired(&self) -> bool {
        matches!(self, LockOutcome::Acquired(_))
    }

    /// Whether the lock is held by another holder.
    pub fn is_held(&self) -> bool {
        matches!(self, LockOutcome::Held { .. })
    }

    /// Whether acquisition failed for an I/O reason other than contention (an
    /// unwritable lock path). (RUNTIME-LOCK-007)
    pub fn is_error(&self) -> bool {
        matches!(self, LockOutcome::Error { .. })
    }
}

/// The injectable clock the TTL/stale logic reads, so tests advance time without
/// real sleeps and the real path uses the wall clock. (RUNTIME-LOCK-001)
pub trait Clock: Send + Sync {
    /// Epoch-seconds "now".
    fn now_secs(&self) -> u64;
}

/// The wall-clock `Clock` (the real path).
#[derive(Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// A test clock whose "now" a test sets explicitly, so the TTL stale-reclaim path
/// is exercised deterministically (no real `sleep`). (RUNTIME-LOCK-001)
#[derive(Clone, Default)]
pub struct ManualClock {
    now: Arc<Mutex<u64>>,
}

impl ManualClock {
    /// A manual clock starting at `start` epoch-seconds.
    pub fn new(start: u64) -> Self {
        ManualClock {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// Advance the clock by `secs` (so a held lock can be aged past its TTL).
    pub fn advance(&self, secs: u64) {
        *self.now.lock().unwrap() += secs;
    }

    /// Set the clock to an absolute epoch-seconds value.
    pub fn set(&self, secs: u64) {
        *self.now.lock().unwrap() = secs;
    }
}

impl Clock for ManualClock {
    fn now_secs(&self) -> u64 {
        *self.now.lock().unwrap()
    }
}

/// The on-disk lockfile contents: the holder identity and the acquisition epoch-
/// seconds, one per line. Tiny, human-readable, machine-local. (RUNTIME-LOCK-001)
fn encode_lockfile(holder: &str, since: u64) -> String {
    // holder on line 1, since-epoch-seconds on line 2.
    format!("{holder}\n{since}\n")
}

/// Parse a lockfile's contents back to `(holder, since)`. A malformed file is
/// treated as a stale/garbage lock (returns `None`), so a corrupt lockfile never
/// wedges acquisition forever. (RUNTIME-LOCK-001)
fn decode_lockfile(contents: &str) -> Option<(Holder, u64)> {
    let mut lines = contents.lines();
    let holder = lines.next()?.to_string();
    let since = lines.next()?.trim().parse::<u64>().ok()?;
    Some((holder, since))
}

/// The machine-local advisory write-lock over a lockfile under the cache dir.
/// Non-blocking `try_acquire`, TTL stale-reclaim, holder identity. Generic over a
/// [`Clock`] so the TTL path is deterministically testable. (RUNTIME-LOCK-001)
pub struct AdvisoryLock<C: Clock = SystemClock> {
    /// The lockfile path (under the cache dir).
    path: PathBuf,
    /// This process's holder identity, recorded into the lockfile on acquire.
    holder: Holder,
    /// The TTL after which a lockfile is stale (its holder presumably crashed)
    /// and reclaimable. (RUNTIME-LOCK-001)
    ttl_secs: u64,
    /// The clock the TTL logic reads.
    clock: C,
}

impl AdvisoryLock<SystemClock> {
    /// The advisory lock over `<cache_dir>/portfolio-tracker.write.lock`, with the
    /// default TTL and the wall clock. (RUNTIME-LOCK-001)
    pub fn in_cache_dir(cache_dir: impl AsRef<Path>, holder: impl Into<Holder>) -> Self {
        AdvisoryLock {
            path: cache_dir.as_ref().join(LOCKFILE_NAME),
            holder: holder.into(),
            ttl_secs: DEFAULT_TTL_SECS,
            clock: SystemClock,
        }
    }
}

impl<C: Clock> AdvisoryLock<C> {
    /// Construct over an explicit lockfile path, TTL, and clock (the testable
    /// constructor). (RUNTIME-LOCK-001)
    pub fn with_clock(
        path: impl Into<PathBuf>,
        holder: impl Into<Holder>,
        ttl_secs: u64,
        clock: C,
    ) -> Self {
        AdvisoryLock {
            path: path.into(),
            holder: holder.into(),
            ttl_secs,
            clock,
        }
    }

    /// This lock's holder identity.
    pub fn holder(&self) -> &str {
        &self.holder
    }

    /// The lockfile path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Non-blocking acquire. **Atomic w.r.t. concurrent processes**: creates the
    /// lockfile via `O_EXCL` (`create_new`) so exactly one of N racing creators wins
    /// — there is no read-then-write window in which two processes both observe the
    /// file absent and both claim it. If creation fails because the file already
    /// exists, re-reads it to decide live-vs-stale: a live lock held by ANOTHER
    /// holder returns the rich `Held{holder, since}`; a live lock held by THIS holder
    /// is **re-entrant** — it returns `Acquired` with a borrowing handle (no-op Drop)
    /// so `import` can hold the lock for the whole commit while its inner writes
    /// re-acquire through the primitive (RUNTIME-LOCK-002/003); a stale (past-TTL) or
    /// malformed lock is reclaimed by **removing the stale file and re-attempting the
    /// same `O_EXCL` create** — so a reclaim is just another exclusive create and only
    /// one of N concurrent reclaimers can win. Never blocks. (RUNTIME-LOCK-001/003)
    ///
    /// **Recovery outcomes** (RUNTIME-LOCK-007):
    /// - *stale past TTL* — reclaim atomically and grant.
    /// - *corrupt / unreadable holder record* — once its on-disk mtime is itself past
    ///   the TTL, treat as not validly held and reclaim atomically rather than refuse
    ///   forever (a freshly-created-but-not-yet-written file is treated as `Held`, see
    ///   next case).
    /// - *mid-initialization* (present but not yet bearing a complete holder/timestamp
    ///   record, mtime within TTL) — treat as `Held` (`<initializing>`), so an
    ///   in-progress acquisition by another process is not stolen.
    /// - *unwritable lock path* (the acquire fails for an I/O reason other than
    ///   contention) — return [`LockOutcome::Error`], never silently `Acquired` and
    ///   never a misleading `Held`.
    // @spec RUNTIME-LOCK-007
    pub fn try_acquire(&self) -> LockOutcome {
        let now = self.clock.now_secs();

        // The parent dir (the cache dir) must exist before we can create the file.
        // If it cannot be created, the lock path is unwritable — surface an
        // acquisition ERROR rather than silently proceeding or falsely claiming the
        // lock is Held by another holder. (RUNTIME-LOCK-007)
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return LockOutcome::Error {
                        reason: format!("lock dir unwritable: {e}"),
                    };
                }
            }
        }

        // A small bounded loop: an exclusive create can lose to a concurrent
        // creator (→ AlreadyExists), and a stale reclaim removes the file and
        // retries the create. The loop terminates because each iteration either
        // acquires, returns Held (live), or removes a stale file then retries; a
        // pathological remove/create race is bounded by the attempt cap.
        for _ in 0..8 {
            // Atomic create: O_EXCL fails with AlreadyExists if any concurrent
            // creator already published the file, so exactly one racer wins the free
            // case. No read-decide-write TOCTOU window. (RUNTIME-LOCK-001)
            match self.create_exclusive(now) {
                Ok(handle) => return LockOutcome::Acquired(handle),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // The file exists — decide live vs. stale below.
                }
                // Any other create error (e.g. an unwritable lock path / a parent
                // that exists but is not a writable directory) leaves the lock
                // unacquired. This is NOT contention — surface an acquisition ERROR
                // to the caller rather than silently proceeding as if Acquired or
                // falsely claiming the lock is Held by another holder (which would
                // mislead a held-lock policy into waiting forever). (RUNTIME-LOCK-007)
                Err(e) => {
                    return LockOutcome::Error {
                        reason: format!("lock path unwritable: {e}"),
                    }
                }
            }

            // The file already exists. Decode it to decide whether it is live or
            // stale. A malformed/garbage lockfile is treated as stale (reclaimable),
            // so a corrupt file never wedges acquisition forever.
            let reclaimable = match fs::read_to_string(&self.path) {
                Ok(contents) => match decode_lockfile(&contents) {
                    Some((holder, since)) => {
                        // A future `since` (clock skew) is treated as live.
                        if now.saturating_sub(since) < self.ttl_secs {
                            // Live. RE-ENTRANT: if THIS holder already holds the lock,
                            // a nested in-primitive acquire (RUNTIME-LOCK-002) must
                            // succeed so `import` can hold the lock for the WHOLE
                            // commit while its inner writes re-acquire through the
                            // primitive (RUNTIME-LOCK-003). The re-entrant handle does
                            // NOT own the file: its `Drop` is a no-op (it carries the
                            // file's own `since`, so the identity check leaves the
                            // outer holder's file intact). Only the original
                            // acquisition's handle releases.
                            if holder == self.holder {
                                return LockOutcome::Acquired(LockHandle {
                                    path: self.path.clone(),
                                    holder: self.holder.clone(),
                                    since,
                                    reentrant: true,
                                });
                            }
                            // Live and held by ANOTHER holder. Return the rich outcome.
                            return LockOutcome::Held { holder, since };
                        }
                        // Stale (its holder crashed): reclaimable.
                        true
                    }
                    // Malformed / empty. This can be a genuinely garbage file OR the
                    // brief window between a concurrent acquirer's O_EXCL create and
                    // its content write (the file exists but is not yet decodable). To
                    // avoid stomping a freshly-created lock, only treat a non-decoding
                    // file as reclaimable once its on-disk mtime is itself older than
                    // the TTL; a recently-touched empty file is treated as live (a
                    // concurrent acquirer mid-write), so the loser backs off as Held.
                    None => {
                        if self.file_age_exceeds_ttl(now) {
                            true
                        } else {
                            return LockOutcome::Held {
                                holder: "<initializing>".to_string(),
                                since: now,
                            };
                        }
                    }
                },
                // The file vanished between the failed create and this read (a racer
                // released it) — retry the exclusive create.
                Err(_) => continue,
            };

            if reclaimable {
                // Reclaim by removing the stale file and looping to re-attempt the
                // exclusive create, so a reclaim is itself an O_EXCL create and only
                // one of N concurrent reclaimers wins. If the remove fails because
                // someone else already removed/replaced it, the next create attempt
                // resolves the race. (RUNTIME-LOCK-001)
                let _ = fs::remove_file(&self.path);
                continue;
            }
        }

        // The bounded loop did not converge (persistent contention). Report Held so
        // the caller retries rather than falsely claiming the lock.
        LockOutcome::Held {
            holder: "<contended>".to_string(),
            since: now,
        }
    }

    /// Whether the lockfile's on-disk modification time is older than the TTL as of
    /// `now` (epoch-seconds). Used to decide whether a NON-decoding file is genuinely
    /// stale garbage (reclaimable) vs. the brief window of a concurrent acquirer's
    /// O_EXCL create before its content write (recently touched → not reclaimable, so
    /// a racer does not stomp a fresh lock). A missing mtime is treated as old
    /// (reclaimable), so a truly corrupt file never wedges acquisition. (RUNTIME-LOCK-001)
    fn file_age_exceeds_ttl(&self, now: u64) -> bool {
        let mtime_secs = fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        match mtime_secs {
            Some(mtime) => now.saturating_sub(mtime) >= self.ttl_secs,
            None => true, // no mtime → treat as old (reclaimable garbage)
        }
    }

    /// Create the lockfile exclusively (`O_EXCL`), recording this holder + `now`.
    /// Returns `AlreadyExists` if a concurrent creator beat us to it. The single
    /// atomic acquire site (free case and post-reclaim retry both go through it).
    /// (RUNTIME-LOCK-001)
    fn create_exclusive(&self, now: u64) -> std::io::Result<LockHandle> {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true) // O_EXCL: atomic create-if-absent across processes.
            .open(&self.path)?;
        f.write_all(encode_lockfile(&self.holder, now).as_bytes())?;
        Ok(LockHandle {
            path: self.path.clone(),
            holder: self.holder.clone(),
            since: now,
            reentrant: false,
        })
    }

    /// Read the current holder of the lock (if held and live as of the clock),
    /// without acquiring — for diagnostics / a writer's held-policy message.
    /// A stale (past-TTL) or absent lock reads as `None`. (RUNTIME-LOCK-003)
    pub fn current_holder(&self) -> Option<(Holder, u64)> {
        let now = self.clock.now_secs();
        let contents = fs::read_to_string(&self.path).ok()?;
        let (holder, since) = decode_lockfile(&contents)?;
        if now.saturating_sub(since) < self.ttl_secs {
            Some((holder, since))
        } else {
            None
        }
    }
}

/// An acquired-lock handle. Owns the lockfile; its `Drop` releases the lock by
/// removing the file (the explicit release the design names). A **re-entrant**
/// handle (a same-holder nested acquire — RUNTIME-LOCK-002/003) does NOT own the
/// file: its `Drop` is a no-op, so only the ORIGINAL acquisition releases.
/// (RUNTIME-LOCK-001)
#[derive(Debug)]
pub struct LockHandle {
    path: PathBuf,
    holder: Holder,
    since: u64,
    /// `true` when this handle is a same-holder re-entrant acquire (it borrows the
    /// lock the original handle owns); its `Drop` must NOT remove the lockfile.
    reentrant: bool,
}

impl LockHandle {
    /// The holder identity recorded for this acquisition.
    pub fn holder(&self) -> &str {
        &self.holder
    }

    /// The acquisition epoch-seconds.
    pub fn since(&self) -> u64 {
        self.since
    }

    /// Consume the handle, triggering `Drop`'s identity-checked release now (instead
    /// of waiting for the handle to fall out of scope). The actual removal — only if
    /// the lockfile still records THIS holder — lives in `Drop`. (RUNTIME-LOCK-001)
    pub fn release(self) {
        // `self` is consumed here, so `Drop` (with its identity check) fires now.
    }
}

impl Drop for LockHandle {
    fn drop(&mut self) {
        // A re-entrant (same-holder nested) handle borrows the lock the ORIGINAL
        // acquisition owns — it must never remove the file, so the outer holder keeps
        // the lock for the whole commit. (RUNTIME-LOCK-002/003)
        if self.reentrant {
            return;
        }
        // Release only if the lockfile still records THIS holder + since (so a
        // crashed-then-reclaimed lock's original holder waking up does not delete
        // the new owner's lockfile). (RUNTIME-LOCK-001)
        if let Ok(contents) = fs::read_to_string(&self.path) {
            if let Some((holder, since)) = decode_lockfile(&contents) {
                if holder == self.holder && since == self.since {
                    let _ = fs::remove_file(&self.path);
                }
            }
        }
    }
}

/// Adapts the runtime [`AdvisoryLock`] to the shared [`pt_core::Lock`] trait, so
/// the SAME machine-local advisory lock is acquired INSIDE every workbook-write
/// primitive that takes a `Lock` (event append via `store`, History append via
/// `reports::append_snapshot`, `sheets_view::republish`, config `put_*`, and the
/// import commit) — coverage independent of the caller (RUNTIME-LOCK-002).
///
/// [`pt_core::Lock::acquire`] is a *fail-or-succeed* surface (no rich `Held`
/// payload), so a `Held` advisory outcome (the lock is held by ANOTHER holder) maps
/// to `Err(LockError::Held)` and an unwritable-path acquisition failure maps to
/// `Err(LockError::Io)` — each crate maps these to its own "return control to the
/// owner, leave no partial state" error (`store` to `StoreError::Unreachable`). A
/// SAME-holder re-acquire is re-entrant and succeeds, so a single process holding
/// the lock for a whole commit (import — RUNTIME-LOCK-006) can drive nested
/// in-primitive writes that re-acquire through this adapter without self-deadlock. A
/// caller wanting the rich `Held{holder, since}` policy (entry's non-destructive
/// fail, summary's read-only fallback) calls [`AdvisoryLock::try_acquire`] directly
/// BEFORE driving the write. (RUNTIME-LOCK-002/003)
pub struct StoreLockAdapter<'a, C: Clock> {
    inner: &'a AdvisoryLock<C>,
}

impl<'a, C: Clock> StoreLockAdapter<'a, C> {
    /// Wrap a runtime advisory lock as a [`pt_core::Lock`].
    pub fn new(inner: &'a AdvisoryLock<C>) -> Self {
        StoreLockAdapter { inner }
    }
}

impl<C: Clock> Lock for StoreLockAdapter<'_, C> {
    fn acquire(&self) -> Result<LockGuard, LockError> {
        match self.inner.try_acquire() {
            // Acquired: hand the write primitive a guard whose Drop releases the
            // lockfile, so the critical section (read-max / append / read-back) is
            // single-writer across processes. (RUNTIME-LOCK-002)
            LockOutcome::Acquired(handle) => {
                let mut held = Some(handle);
                Ok(LockGuard::new(Some(Box::new(move || {
                    // Dropping the moved handle releases the lockfile.
                    held.take();
                }))))
            }
            // An acquisition I/O error (an unwritable lock path) — surface it as an
            // error to the caller rather than silently proceed as if Acquired.
            // (RUNTIME-LOCK-007)
            LockOutcome::Error { .. } => Err(LockError::Io),
            // Held by another process: a write primitive that acquires inside
            // itself must NOT proceed (a cron capture and an open TUI cannot both
            // append). Return control to the owner, leaving no partial state — the
            // same contract as a transport failure. (RUNTIME-LOCK-002/003)
            LockOutcome::Held { .. } => Err(LockError::Held),
        }
    }
}
