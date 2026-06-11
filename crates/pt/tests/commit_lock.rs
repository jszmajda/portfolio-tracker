//! End-to-end advisory-write-lock cascade for `import::commit` driven by the REAL
//! reentrant `runtime::AdvisoryLock` (RUNTIME-LOCK-006, IMPORT-RUN-007). This is
//! the only crate that depends on BOTH `import` and `runtime`, so the
//! reentrancy-without-deadlock and the concurrent-writer-lockout properties are
//! proven here against the genuine lockfile lock (no network: the store rides the
//! `FakeSheetsApi`).
#![allow(clippy::inconsistent_digit_grouping)]

use std::path::PathBuf;

use import::testkit::{buy_row, founding_residency, position_row, sale_row};
use import::{commit, dry_run, LegacyWorkbook};
use ledger_core::Marks;
use pt_core::Date;
use runtime::lock::{AdvisoryLock, LockOutcome};
use runtime::{ManualClock, StoreLockAdapter, DEFAULT_TTL_SECS, LOCKFILE_NAME};
use store::testkit::InMemorySheets;
use store::{InMemoryCache, SheetsClient, Store, Tab};

/// A unique temp lockfile path for one test (so tests don't share lockfiles).
fn temp_lockfile(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "pt-commit-lock-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p.push(LOCKFILE_NAME);
    let _ = std::fs::remove_file(&p);
    p
}

fn date(y: i32, m: i32, d: i32) -> Date {
    // Days since 1970-01-01 (civil), matching the import fixtures.
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m - 3) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Date(era * 146_097 + doe - 719_468)
}

/// A committable single-symbol GOOG stream (a Buy then a Sell) that reconciles as
/// MATCHED against its legacy `Positions` row. Mirrors the `import` GOOG fixture.
fn committable_workbook() -> LegacyWorkbook {
    const PLATFORM: &str = "schwab";
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(buy_row(
        "Stock Actions",
        3,
        "GOOG-B1",
        "GOOG",
        100,
        "50.00",
        "0",
        date(2020, 1, 15),
        PLATFORM,
    ));
    wb.sales.push(sale_row(
        "Stock Sales",
        3,
        "GOOG-S1",
        "GOOG-B1",
        "GOOG",
        20,
        "55.00",
        "0",
        date(2021, 6, 1),
        PLATFORM,
    ));
    wb.positions.push(position_row("GOOG", 80, 10_000, 80_000));
    wb
}

// @spec RUNTIME-LOCK-006, IMPORT-RUN-007
#[test]
fn commit_with_a_real_reentrant_lock_succeeds_without_deadlock() {
    // The commit HOLDS the whole-commit lock (the outer acquire before the target
    // check) while each inner `store` append RE-ACQUIRES the SAME machine-local lock
    // as the SAME holder. With the genuine reentrant AdvisoryLock this must NOT
    // deadlock — the nested acquire returns Acquired against the same holder, and the
    // whole commit lands. (RUNTIME-LOCK-006, IMPORT-RUN-007)
    let path = temp_lockfile("reentrant-commit");
    let clock = ManualClock::new(10_000);
    let lock = AdvisoryLock::with_clock(&path, "import-commit", DEFAULT_TTL_SECS, clock);

    let mut store = Store::new(
        InMemorySheets::new(),
        StoreLockAdapter::new(&lock),
        InMemoryCache::new(),
    );

    let report = dry_run(&committable_workbook(), &Marks::new()).expect("dry-run");
    assert!(report.commit_allowed);

    let out = commit(&mut store, &report.accept())
        .expect("a commit under a free reentrant lock succeeds");
    assert_eq!(
        out.appended.len(),
        2,
        "both reconstructed ledger events landed"
    );
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        2,
        "the workbook holds the two events after the whole-commit-locked commit"
    );

    // After the commit returns, the whole-commit guard has dropped — the lock is
    // free again (no leaked hold).
    let probe =
        AdvisoryLock::with_clock(&path, "probe", DEFAULT_TTL_SECS, ManualClock::new(10_000));
    assert!(
        probe.try_acquire().is_acquired(),
        "the whole-commit guard released when commit returned"
    );
    let _ = std::fs::remove_file(&path);
}

// @spec RUNTIME-LOCK-006, IMPORT-RUN-007
#[test]
fn commit_is_refused_when_another_writer_holds_the_lock_no_toctou() {
    // Another writer (a DIFFERENT holder) holds the lock for the whole test. The
    // commit's whole-commit acquire — taken BEFORE the legacy-target check — sees the
    // lock Held and refuses the commit, so NOTHING is checked-then-appended: the
    // check→append window never opens while a foreign writer holds the lock (no
    // TOCTOU), and the workbook is left empty. (RUNTIME-LOCK-006, IMPORT-RUN-007)
    let path = temp_lockfile("toctou-commit");
    let clock = ManualClock::new(20_000);

    // The foreign writer holds the lock.
    let other = AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock.clone());
    let _held = match other.try_acquire() {
        LockOutcome::Acquired(h) => h,
        o => panic!("the foreign writer should acquire, got {o:?}"),
    };

    // The importer's store over a DIFFERENT holder's advisory lock.
    let import_lock = AdvisoryLock::with_clock(&path, "import-commit", DEFAULT_TTL_SECS, clock);
    let mut store = Store::new(
        InMemorySheets::new(),
        StoreLockAdapter::new(&import_lock),
        InMemoryCache::new(),
    );

    let report = dry_run(&committable_workbook(), &Marks::new()).expect("dry-run");
    assert!(report.commit_allowed);

    let result = commit(&mut store, &report.accept());
    assert!(
        result.is_err(),
        "a commit while another writer holds the lock must be refused (no check→append TOCTOU)"
    );
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        0,
        "a refused commit appends nothing"
    );
    let _ = std::fs::remove_file(&path);
}
