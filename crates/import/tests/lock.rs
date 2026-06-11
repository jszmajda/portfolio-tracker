//! The whole-commit advisory-write-lock hold for `import::commit`
//! (IMPORT-RUN-007, RUNTIME-LOCK-006): the commit acquires the lock BEFORE the
//! legacy-target check and holds it across the check + all appends, so no foreign
//! writer interleaves between the check and the appends (no check→append TOCTOU).
//! Because the lock is reentrant for a same holder, the inner append primitives
//! re-acquire it under the held lock without deadlock.
//!
//! Driven with the in-memory `store` fakes and the acquire-counting `NoopLock`
//! (no network). The REAL reentrant `AdvisoryLock` reentrancy + the
//! concurrent-writer-lockout end-to-end live in `pt/tests/` (the only crate that
//! depends on BOTH `import` and `runtime`).

mod common;
use common::*;

use import::testkit::{fresh_store_with_lock, founding_residency};
use import::{commit, dry_run, LegacyWorkbook};
use ledger_core::Marks;
use store::{SheetsClient, Tab};

// @spec IMPORT-RUN-007, RUNTIME-LOCK-006
#[test]
fn commit_holds_the_lock_across_the_whole_commit_and_inner_appends_reacquire() {
    // A committable GOOG stream: a Buy then a Sell (two ledger appends, no tax).
    let mut wb = LegacyWorkbook {
        residency: founding_residency("DC", date(2015, 1, 1)),
        ..LegacyWorkbook::default()
    };
    wb.actions.push(goog_buy());
    wb.sales.push(goog_sale());
    wb.positions.push(goog_position_legacy());

    let report = dry_run(&wb, &Marks::new()).expect("dry-run");
    assert!(report.commit_allowed);
    let ledger_count = report.reconstruction.ledger.len();
    let tax_count = report.reconstruction.tax.len();
    assert_eq!(ledger_count, 2, "the GOOG stream is a Buy then a Sell");

    let (mut store, lock) = fresh_store_with_lock();
    let commit_report = commit(&mut store, &report.accept()).expect("commit to a fresh store");
    assert_eq!(commit_report.appended.len(), 2, "two ledger events appended");
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        2,
        "the workbook now holds the two reconstructed events"
    );

    // The commit acquired the SAME advisory lock once for the WHOLE commit (the
    // outer hold, before the target check) PLUS once inside each inner append
    // primitive (re-acquired re-entrantly under the held lock — no deadlock). So
    // the total acquire count is 1 (whole-commit) + ledger appends + tax appends.
    // (IMPORT-RUN-007, RUNTIME-LOCK-006)
    assert_eq!(
        lock.acquire_count(),
        1 + (ledger_count as u32) + (tax_count as u32),
        "commit holds the lock across the whole commit while inner appends re-acquire it"
    );
}
