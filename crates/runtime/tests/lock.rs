//! The cross-process advisory write-lock: RUNTIME-LOCK-001/002/003. Driven with
//! the FAKE clock + a temp-dir lockfile (no real sleeps, no network).
#![allow(clippy::inconsistent_digit_grouping)]

use std::path::PathBuf;

use runtime::lock::{AdvisoryLock, LockOutcome};
use runtime::{ManualClock, StoreLockAdapter, DEFAULT_TTL_SECS, LOCKFILE_NAME};
use store::Lock;

/// A unique temp lockfile path for one test (so tests don't share lockfiles).
fn temp_lockfile(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "pt-runtime-test-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p.push(LOCKFILE_NAME);
    // Clean any prior leftover.
    let _ = std::fs::remove_file(&p);
    p
}

// @spec RUNTIME-LOCK-001
#[test]
fn try_acquire_is_acquired_when_free() {
    let path = temp_lockfile("free");
    let clock = ManualClock::new(1_000);
    let lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, clock);

    let outcome = lock.try_acquire();
    assert!(outcome.is_acquired(), "a free lock acquires");
    // The lockfile now exists, recording this holder.
    assert!(path.exists(), "acquiring writes the lockfile");
    if let LockOutcome::Acquired(handle) = outcome {
        assert_eq!(handle.holder(), "tui-1");
        assert_eq!(handle.since(), 1_000);
    }
}

// @spec RUNTIME-LOCK-001
#[test]
fn try_acquire_returns_held_with_holder_and_since_when_taken() {
    let path = temp_lockfile("held");
    let clock = ManualClock::new(2_000);

    // Holder A acquires and keeps the lock (handle stays alive).
    let lock_a = AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock.clone());
    let held = lock_a.try_acquire();
    assert!(held.is_acquired());
    let _guard_a = match held {
        LockOutcome::Acquired(h) => h,
        _ => panic!("A should acquire"),
    };

    // Holder B (a different process) tries while A holds it: non-blocking Held,
    // naming the holder and the since-epoch.
    let lock_b = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, clock.clone());
    match lock_b.try_acquire() {
        LockOutcome::Held { holder, since } => {
            assert_eq!(holder, "cron-summary", "Held names the current holder");
            assert_eq!(since, 2_000, "Held carries the acquisition epoch");
        }
        LockOutcome::Acquired(_) => panic!("B must NOT acquire while A holds"),
        LockOutcome::Error { reason } => panic!("unexpected acquisition error: {reason}"),
    }
}

// @spec RUNTIME-LOCK-001
#[test]
fn explicit_release_frees_the_lock() {
    let path = temp_lockfile("release");
    let clock = ManualClock::new(3_000);
    let lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, clock);

    let handle = match lock.try_acquire() {
        LockOutcome::Acquired(h) => h,
        _ => panic!("acquire"),
    };
    handle.release(); // explicit release (drops the handle, removes the lockfile)
    assert!(!path.exists(), "explicit release removes the lockfile");

    // A subsequent acquire succeeds (the lock is free again).
    assert!(lock.try_acquire().is_acquired(), "re-acquire after release");
}

// @spec RUNTIME-LOCK-001, RUNTIME-LOCK-007
#[test]
fn ttl_reclaims_a_stale_crashed_holder_lock() {
    let path = temp_lockfile("ttl");
    let ttl = 300;
    let clock = ManualClock::new(10_000);

    // A "crashed" holder leaves a lockfile behind (we leak the handle to model the
    // process dying without releasing).
    let crashed = AdvisoryLock::with_clock(&path, "crashed-tui", ttl, clock.clone());
    let leaked = match crashed.try_acquire() {
        LockOutcome::Acquired(h) => h,
        _ => panic!("crashed holder acquires"),
    };
    std::mem::forget(leaked); // the process died; the lockfile is orphaned.

    // Before the TTL elapses, the lock is still Held (the holder might be alive).
    let cron = AdvisoryLock::with_clock(&path, "cron-summary", ttl, clock.clone());
    assert!(
        cron.try_acquire().is_held(),
        "within TTL the lock is still held"
    );

    // After the TTL elapses, the stale lock is reclaimable — cron acquires it, so a
    // crashed TUI cannot wedge the cron summary forever.
    clock.advance(ttl + 1);
    match cron.try_acquire() {
        LockOutcome::Acquired(h) => assert_eq!(h.holder(), "cron-summary"),
        LockOutcome::Held { .. } => panic!("a stale lock past the TTL must be reclaimable"),
        LockOutcome::Error { reason } => panic!("unexpected acquisition error: {reason}"),
    }
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-LOCK-001
#[test]
fn exactly_one_of_many_concurrent_reclaimers_of_a_stale_lock_wins() {
    // A crashed holder leaves a stale lockfile; several live processes then race to
    // reclaim it past the TTL. The reclaim is itself an atomic O_EXCL create (remove-
    // then-create), so EXACTLY ONE reclaimer wins — never two, which a rename-over /
    // in-place overwrite would permit. (RUNTIME-LOCK-001)
    use std::sync::{Arc, Barrier};

    let path = temp_lockfile("reclaim-race");
    // Seed a stale lockfile by hand: a crashed holder at epoch 0, far past any TTL.
    let _ = std::fs::write(&path, "crashed-holder\n0\n");

    const N: usize = 12;
    let ttl = 300u64;
    let barrier = Arc::new(Barrier::new(N));
    let mut handles = Vec::new();
    for i in 0..N {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            // A fixed "now" well past the stale lock's epoch-0 + ttl, so every thread
            // agrees the seeded lock is reclaimable and they genuinely race.
            let clock = ManualClock::new(1_000_000);
            let lock = AdvisoryLock::with_clock(&path, format!("reclaimer-{i}"), ttl, clock);
            barrier.wait();
            match lock.try_acquire() {
                LockOutcome::Acquired(h) => {
                    std::mem::forget(h);
                    1usize
                }
                LockOutcome::Held { .. } => 0usize,
                LockOutcome::Error { .. } => 0usize,
            }
        }));
    }

    let winners: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(winners, 1, "exactly one reclaimer of a stale lock wins");
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-LOCK-001, RUNTIME-LOCK-004
#[test]
fn exactly_one_of_many_concurrent_acquirers_wins() {
    // The real contention is two PROCESSES racing for the same lockfile (cron
    // summary vs. an open TUI). The acquire must be atomic across them: of N threads
    // (modeling N processes) racing `try_acquire` on the SAME free lockfile, EXACTLY
    // ONE returns Acquired — never two, which an in-place read-then-write would
    // permit (both observe the file absent, both write, both "acquire").
    // (RUNTIME-LOCK-001)
    use std::sync::{Arc, Barrier};

    let path = temp_lockfile("contention");
    // Use the system clock here: the threads run concurrently in wall time, and the
    // acquire's atomicity (O_EXCL), not the TTL, is what is under test. The TTL is
    // long so no thread reclaims another's fresh lock as stale.
    const N: usize = 16;
    let barrier = Arc::new(Barrier::new(N));
    let mut handles = Vec::new();
    for i in 0..N {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let clock = runtime::SystemClock;
            let lock =
                AdvisoryLock::with_clock(&path, format!("racer-{i}"), DEFAULT_TTL_SECS, clock);
            // Line every thread up so they hit `try_acquire` as simultaneously as the
            // scheduler allows — maximizing the chance to expose a TOCTOU race.
            barrier.wait();
            match lock.try_acquire() {
                // Keep the handle alive (and leak it) so the winner's lockfile is not
                // released before the losers observe it Held. We only count winners.
                LockOutcome::Acquired(h) => {
                    std::mem::forget(h);
                    1usize
                }
                LockOutcome::Held { .. } => 0usize,
                LockOutcome::Error { .. } => 0usize,
            }
        }));
    }

    let winners: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(
        winners, 1,
        "exactly one concurrent acquirer wins the free lock"
    );
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-LOCK-002
#[test]
fn store_lock_adapter_acquires_inside_a_write_primitive() {
    // The adapter exposes the advisory lock as store's `Lock`, so a write primitive
    // that acquires INSIDE itself takes the SAME machine-local lock — coverage
    // independent of the caller. When free, the store-side acquire succeeds and the
    // guard's Drop releases it. (RUNTIME-LOCK-002)
    let path = temp_lockfile("adapter-free");
    let clock = ManualClock::new(5_000);
    let lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, clock.clone());
    let adapter = StoreLockAdapter::new(&lock);
    // A DIFFERENT holder models a concurrent process (a same-holder re-acquire is
    // re-entrant, so the concurrency probe must use a distinct identity).
    let concurrent = AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock);

    {
        let _guard = adapter
            .acquire()
            .expect("store-side acquire succeeds when free");
        // While the guard is alive, the lockfile is held (a concurrent process sees Held).
        assert!(
            concurrent.try_acquire().is_held(),
            "the in-primitive guard holds the lock"
        );
    }
    // After the guard drops (the critical section ends), the lock is free again.
    assert!(
        concurrent.try_acquire().is_acquired(),
        "guard Drop releases the in-primitive lock"
    );
}

// @spec RUNTIME-LOCK-003, RUNTIME-LOCK-005
#[test]
fn same_holder_reacquire_is_reentrant_so_import_holds_across_nested_writes() {
    // import holds the lock for the WHOLE commit (RUNTIME-LOCK-003) while its inner
    // writes re-acquire INSIDE every write primitive (RUNTIME-LOCK-002). A same-holder
    // re-acquire must therefore be RE-ENTRANT: it returns Acquired (never Held against
    // itself), and releasing the inner (re-entrant) handle must NOT drop the outer
    // holder's lock — only the original acquisition releases. (RUNTIME-LOCK-002/003)
    let path = temp_lockfile("reentrant");
    let clock = ManualClock::new(8_000);
    let lock = AdvisoryLock::with_clock(&path, "import-commit", DEFAULT_TTL_SECS, clock.clone());

    // import acquires the lock for the whole commit (the OUTER hold).
    let outer = match lock.try_acquire() {
        LockOutcome::Acquired(h) => h,
        _ => panic!("import acquires for the whole commit"),
    };

    {
        // A nested in-primitive write re-acquires the SAME lock as the SAME holder.
        let adapter = StoreLockAdapter::new(&lock);
        let nested = adapter
            .acquire()
            .expect("a same-holder re-acquire is re-entrant");
        // A DIFFERENT holder still sees the lock Held meanwhile.
        let other =
            AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock.clone());
        assert!(
            other.try_acquire().is_held(),
            "another process is locked out during the commit"
        );
        drop(nested); // the inner write finishes.
    }

    // After the nested write's guard dropped, the OUTER hold is intact: the lockfile
    // still exists and a different holder is still locked out.
    assert!(
        path.exists(),
        "the re-entrant inner release did not drop import's outer hold"
    );
    let other = AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock.clone());
    assert!(
        other.try_acquire().is_held(),
        "import still holds the lock for the whole commit"
    );

    // Only when import's ORIGINAL handle drops is the lock free.
    drop(outer);
    assert!(
        !path.exists(),
        "the original acquisition's release frees the lock"
    );
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-LOCK-002
#[test]
fn store_lock_adapter_refuses_when_held_by_another_process() {
    // A write primitive acquiring inside itself must NOT proceed while another
    // process holds the lock (a cron capture and an open TUI cannot both append):
    // the store-side acquire errors, returning control to the owner with no partial
    // state. (RUNTIME-LOCK-002)
    let path = temp_lockfile("adapter-held");
    let clock = ManualClock::new(6_000);

    let lock_a = AdvisoryLock::with_clock(&path, "cron", DEFAULT_TTL_SECS, clock.clone());
    let _held = match lock_a.try_acquire() {
        LockOutcome::Acquired(h) => h,
        _ => panic!("A acquires"),
    };

    let lock_b = AdvisoryLock::with_clock(&path, "tui", DEFAULT_TTL_SECS, clock);
    let adapter_b = StoreLockAdapter::new(&lock_b);
    assert!(
        adapter_b.acquire().is_err(),
        "the in-primitive acquire must fail while another process holds the lock"
    );
}

// @spec RUNTIME-LOCK-002
#[test]
fn in_primitive_acquisition_through_a_real_store_write_blocks_a_concurrent_holder() {
    // End-to-end through the REAL adapter, not the trait in isolation: a Store built
    // over StoreSheetsAdapter + StoreLockAdapter acquires the SAME machine-local lock
    // INSIDE its append primitive. While ANOTHER process holds the lockfile, the
    // store's append fails (Unreachable) — proving in-primitive acquisition is
    // complete regardless of the caller. (RUNTIME-LOCK-002)
    use ledger_core::{LedgerEvent, LedgerEventKind};
    use pt_core::{Cents, Date, MicroShares, Seq};
    use runtime::testkit::FakeSheetsApi;
    use runtime::StoreSheetsAdapter;
    use store::{InMemoryCache, Store};

    let path = temp_lockfile("store-inprimitive");
    let clock = ManualClock::new(9_000);

    // Another process (cron) holds the lockfile for the whole test.
    let cron = AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock.clone());
    let _held = match cron.try_acquire() {
        LockOutcome::Acquired(h) => h,
        _ => panic!("cron acquires"),
    };

    // The TUI's store: a real StoreSheetsAdapter over the fake low-level API, and a
    // StoreLockAdapter over the TUI's advisory lock (a DIFFERENT holder).
    let tui_lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, clock);
    let sheets = StoreSheetsAdapter::new(FakeSheetsApi::new());
    let mut store = Store::new(
        sheets,
        StoreLockAdapter::new(&tui_lock),
        InMemoryCache::new(),
    );

    let event = LedgerEvent {
        id: "e1".to_string(),
        seq: Seq(1),
        date: Date(19_000),
        kind: LedgerEventKind::Buy {
            lot_id: "lot-1".to_string(),
            symbol: "AMZN".to_string(),
            qty: MicroShares(1_000_000),
            unit_price_cents: Cents(150_00),
            fees_cents: Cents(0),
            platform: "schwab".to_string(),
            tracking_code: None,
        },
    };

    // The in-primitive lock acquire sees the lock Held by cron → the append fails
    // (Unreachable), leaving no partial state. (RUNTIME-LOCK-002)
    match store.append_ledger(&event) {
        Err(store::StoreError::Unreachable) => {}
        other => panic!("a write while another process holds the lock must fail, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-LOCK-003
#[test]
fn held_outcome_drives_per_writer_policy() {
    // When Held, the rich outcome (holder + since) is returned to the writer, which
    // applies its policy. This test exercises the discrimination the policies ride
    // on: entry fails non-destructively, summary runs read-only — both branch on
    // `is_held()` and read `holder`/`since`. (RUNTIME-LOCK-003)
    let path = temp_lockfile("policy");
    let clock = ManualClock::new(7_000);

    let holder = AdvisoryLock::with_clock(&path, "import-commit", DEFAULT_TTL_SECS, clock.clone());
    let _held = match holder.try_acquire() {
        LockOutcome::Acquired(h) => h,
        _ => panic!("import acquires for the whole commit"),
    };

    // entry's policy: on Held, fail the submit non-destructively (preserve entry).
    let entry_lock = AdvisoryLock::with_clock(&path, "tui-entry", DEFAULT_TTL_SECS, clock.clone());
    let entry_can_write = match entry_lock.try_acquire() {
        LockOutcome::Acquired(_) => true,
        LockOutcome::Held { holder, since } => {
            // The writer sees who holds it and how long — the policy's message.
            assert_eq!(holder, "import-commit");
            assert_eq!(since, 7_000);
            false
        }
        LockOutcome::Error { reason } => panic!("unexpected acquisition error: {reason}"),
    };
    assert!(
        !entry_can_write,
        "entry must fail non-destructively while import holds"
    );

    // summary's policy: on Held, run read-only (skip the capture, print from cache).
    let summary_lock = AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock);
    let summary_runs_readonly = summary_lock.try_acquire().is_held();
    assert!(
        summary_runs_readonly,
        "summary runs read-only while import holds"
    );
}

// ===========================================================================
// RUNTIME-LOCK-007..010 recovery outcomes. The stale-past-TTL reclaim
// (RUNTIME-LOCK-007) is covered by `ttl_reclaims_a_stale_crashed_holder_lock`
// above; these add the corrupt/unreadable (RUNTIME-LOCK-008),
// mid-initialization (RUNTIME-LOCK-009), and unwritable-path
// (RUNTIME-LOCK-010) cases.
// ===========================================================================

// @spec RUNTIME-LOCK-008
#[test]
fn a_corrupt_unparseable_lockfile_past_its_mtime_ttl_is_reclaimed() {
    // A lockfile whose holder/timestamp record is corrupt/unparseable must NOT wedge
    // acquisition forever: once its on-disk mtime is itself older than the TTL it is
    // treated as not validly held and reclaimed atomically. We seed a garbage file
    // and set the clock far past its (real) mtime so the mtime-age exceeds the TTL.
    let path = temp_lockfile("corrupt-reclaim");
    std::fs::create_dir_all(path.parent().unwrap()).expect("create the lockfile dir");
    std::fs::write(&path, "this-is-not-a-valid-lockfile-record").expect("seed corrupt file");

    let ttl = 300u64;
    // The seeded file's real mtime is ~now (wall clock). A ManualClock far in the
    // future makes `now - mtime` exceed the TTL, so the corrupt file reads as stale.
    let future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 10_000;
    let clock = ManualClock::new(future);
    let lock = AdvisoryLock::with_clock(&path, "cron-summary", ttl, clock);

    match lock.try_acquire() {
        LockOutcome::Acquired(h) => assert_eq!(h.holder(), "cron-summary"),
        LockOutcome::Held { holder, .. } => {
            panic!("a corrupt lockfile past its mtime-TTL must be reclaimed, got Held by {holder}")
        }
        LockOutcome::Error { reason } => panic!("unexpected acquisition error: {reason}"),
    }
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-LOCK-009
#[test]
fn a_mid_initialization_lockfile_is_treated_as_held_not_stolen() {
    // A lockfile that EXISTS but does not yet bear a complete holder/timestamp record
    // (the brief window between a concurrent acquirer's O_EXCL create and its content
    // write) and whose mtime is within the TTL must be treated as `Held` — an
    // in-progress acquisition by another process must not be stolen.
    let path = temp_lockfile("mid-init");
    std::fs::create_dir_all(path.parent().unwrap()).expect("create the lockfile dir");
    // An empty (not-yet-written) file with a fresh mtime models the mid-init window.
    std::fs::write(&path, "").expect("seed an empty mid-init file");

    // The clock is at "now" so the freshly-created file's mtime is within the TTL.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let lock = AdvisoryLock::with_clock(&path, "racer", 300, ManualClock::new(now));

    match lock.try_acquire() {
        LockOutcome::Held { .. } => {} // not stolen — correct.
        LockOutcome::Acquired(_) => {
            panic!("a mid-initialization lockfile must NOT be stolen (treated as Held)")
        }
        LockOutcome::Error { reason } => panic!("unexpected acquisition error: {reason}"),
    }
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-LOCK-010
#[test]
fn an_unwritable_lock_path_surfaces_an_acquisition_error_not_a_silent_acquire() {
    // When the lock path is unwritable (the parent "directory" is actually a regular
    // FILE, so the lockfile cannot be created), acquisition fails for an I/O reason
    // OTHER than contention. It must surface as an acquisition ERROR — never silently
    // `Acquired`, and never a misleading `Held` (which a held-lock policy would wait
    // on forever).
    let mut file_as_dir = std::env::temp_dir();
    file_as_dir.push(format!(
        "pt-runtime-unwritable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // Create a regular file where the lock's PARENT dir would be.
    std::fs::write(&file_as_dir, "i am a file, not a directory").expect("seed a blocking file");
    let lock_path = file_as_dir.join(LOCKFILE_NAME); // a child path under a non-dir.

    let lock = AdvisoryLock::with_clock(&lock_path, "tui-1", DEFAULT_TTL_SECS, ManualClock::new(1));
    let outcome = lock.try_acquire();
    assert!(
        outcome.is_error(),
        "an unwritable lock path must surface an acquisition error, got {outcome:?}"
    );
    assert!(
        !outcome.is_acquired(),
        "an unwritable path must NEVER read as Acquired"
    );
    assert!(
        !outcome.is_held(),
        "an unwritable path is NOT contention (not Held)"
    );
    let _ = std::fs::remove_file(&file_as_dir);
}
