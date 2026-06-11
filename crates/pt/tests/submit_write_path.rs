//! The interactive TUI's authoritative write path, driven against the FAKE store
//! (`InMemorySheets`) + a REAL reentrant `runtime::AdvisoryLock` — no network. These
//! pin the `SubmitOutcome` contract the TUI composer maps to a `Phase`:
//!
//! - **Confirmed** — re-validate live → assign EventId (RUNTIME-EVENTID-001) →
//!   revalidate against the refreshed view (RUNTIME-REVALIDATE-001) → acquire the lock
//!   → append + read-back-verify lands the event durably. (TUI-ENTRY-FLOW-003)
//! - **Rejected{disagreement}** — a submit-time kernel disagreement against the
//!   refreshed live state is carried back inline, distinct from a write-verify failure;
//!   nothing is written. (TUI-ENTRY-FLOW-002)
//! - **LockHeld** — a foreign writer (a cron `summary`) holds the advisory lock; the
//!   submit fails non-destructively, the holder named, no queue. (TUI-ENTRY-FLOW-005)
//! - **WriteFailed** — an unreachable workbook returns control with `[r]etry`.
//!   (TUI-ENTRY-FLOW-004)
//! - **Idempotent retry** — the byte-identical frozen event re-submits with the SAME
//!   content-hash EventId, so the store idempotency lands it exactly once. (TUI-ENTRY-FLOW-010)
//!
//! These close the live-wiring half of RUNTIME-EVENTID-001 / RUNTIME-REVALIDATE-001:
//! their machinery (the assigners + the revalidate handshake) is wired into a real
//! append here.
#![allow(clippy::inconsistent_digit_grouping)]

use std::path::PathBuf;

use ledger_core::{LedgerError, LedgerEvent, LedgerEventKind, LotRef};
use pt_core::{Cents, Date, MicroShares, Seq};
use runtime::lock::{AdvisoryLock, LockOutcome};
use runtime::{ManualClock, StoreLockAdapter, DEFAULT_TTL_SECS, LOCKFILE_NAME};
use store::testkit::InMemorySheets;
use store::{InMemoryCache, SheetsClient, Store, Tab};
use tax::TaxEvent;

use pt::wiring::{context_for_year, submit_ledger_through_store, submit_tax_through_store};
use tui::port::{SubmitOutcome, SubmitRejection, WriteFailure};

/// A unique temp lockfile for one test (so tests do not share lockfiles).
fn temp_lockfile(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "pt-submit-{}-{}-{}",
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

fn ms(n: i64) -> MicroShares {
    MicroShares(n * pt_core::SHARE_SCALE)
}

/// A store over the FAKE workbook + the SAME advisory lock the submit path drives
/// (adapted into the store's `Lock` slot, so the inner append re-acquires re-entrantly
/// against the same holder).
fn store_over<'a>(
    sheets: InMemorySheets,
    lock: &'a AdvisoryLock<ManualClock>,
) -> Store<InMemorySheets, StoreLockAdapter<'a, ManualClock>, InMemoryCache> {
    Store::new(sheets, StoreLockAdapter::new(lock), InMemoryCache::new())
}

/// A Buy candidate with NO id (the composer carries a placeholder; the id is
/// store-assigned via RUNTIME-EVENTID-001 at submit). Frozen content for the retry.
fn a_buy() -> LedgerEvent {
    LedgerEvent {
        id: String::new(),
        seq: Seq(0),
        date: Date(20_000),
        kind: LedgerEventKind::Buy {
            lot_id: "L1".to_string(),
            symbol: "AMZN".to_string(),
            qty: ms(10),
            unit_price_cents: Cents(10_000),
            fees_cents: Cents(0),
            platform: "Robinhood".to_string(),
            tracking_code: None,
        },
    }
}

// @spec TUI-ENTRY-FLOW-003, RUNTIME-EVENTID-001, RUNTIME-REVALIDATE-001
#[test]
fn confirmed_durable_lands_the_event_with_an_assigned_event_id() {
    // The happy path: a Buy re-validates live against the (empty) refreshed log, gets a
    // store-assigned content-hash EventId, acquires the free advisory lock, and appends
    // + read-back-verifies — Confirmed, exactly one row landed. (TUI-ENTRY-FLOW-003)
    let path = temp_lockfile("confirmed");
    let lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, ManualClock::new(10_000));
    let mut store = store_over(InMemorySheets::new(), &lock);

    let outcome = submit_ledger_through_store(&mut store, &lock, &a_buy());
    match outcome {
        SubmitOutcome::Confirmed(o) => {
            assert!(
                !o.event_id.is_empty(),
                "an EventId was assigned (RUNTIME-EVENTID-001)"
            );
            assert!(
                !o.idempotent_skip,
                "the first submit is a real append, not a skip"
            );
        }
        other => panic!("expected Confirmed, got {other:?}"),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        1,
        "the event landed durably (read-back-verified)"
    );
    // The lock released when the submit returned (no leaked hold).
    let probe =
        AdvisoryLock::with_clock(&path, "probe", DEFAULT_TTL_SECS, ManualClock::new(10_000));
    assert!(
        probe.try_acquire().is_acquired(),
        "the advisory lock released after the submit"
    );
    let _ = std::fs::remove_file(&path);
}

// @spec TUI-ENTRY-FLOW-002, RUNTIME-REVALIDATE-001
#[test]
fn submit_revalidates_live_and_rejects_a_kernel_disagreement_inline() {
    // A Sell of 100 shares of a lot that holds only 10 (per the refreshed live log)
    // re-validates against the live state at submit and the kernel rejects it
    // (InsufficientShares). The specific kernel error is carried back inline, distinct
    // from a write-verify failure; nothing is written. (TUI-ENTRY-FLOW-002)
    let path = temp_lockfile("rejected");
    let lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, ManualClock::new(10_000));
    // Seed a workbook with a Buy of 10 AMZN (so the live log holds 10 shares).
    let mut store = store_over(InMemorySheets::new(), &lock);
    let confirmed = submit_ledger_through_store(&mut store, &lock, &a_buy());
    assert!(
        matches!(confirmed, SubmitOutcome::Confirmed(_)),
        "seed Buy lands"
    );

    // A Sell of 100 (more than the 10 held) against the live log — over-allocated, so
    // the kernel rejects at submit-time re-validation. (TUI-ENTRY-FLOW-002)
    let oversell = LedgerEvent {
        id: String::new(),
        seq: Seq(0),
        date: Date(20_001),
        kind: LedgerEventKind::Sell {
            sale_id: "S1".to_string(),
            symbol: "AMZN".to_string(),
            qty: ms(100),
            unit_price_cents: Cents(11_000),
            fees_cents: Cents(0),
            lot_refs: vec![LotRef {
                lot_id: "L1".to_string(),
                qty: ms(100),
            }],
            accrues_to_state: Some("DC".to_string()),
            platform: "Robinhood".to_string(),
            tracking_code: None,
        },
    };
    let outcome = submit_ledger_through_store(&mut store, &lock, &oversell);
    match outcome {
        SubmitOutcome::Rejected(SubmitRejection::Ledger(e)) => {
            assert_eq!(
                e,
                LedgerError::InsufficientShares,
                "the specific kernel error rides back inline"
            );
        }
        other => panic!("expected a live-revalidation Rejected, got {other:?}"),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        1,
        "a submit-time rejection writes nothing (only the seed Buy remains)"
    );
    let _ = std::fs::remove_file(&path);
}

// @spec TUI-ENTRY-FLOW-005, RUNTIME-LOCK-003
#[test]
fn lock_held_by_a_foreign_writer_fails_non_destructively_naming_the_holder() {
    // A cron `summary` (a DIFFERENT holder) holds the advisory lock. The TUI submit's
    // rich try-acquire sees it Held and fails non-destructively, naming the holder — no
    // queue, nothing written. (TUI-ENTRY-FLOW-005)
    let path = temp_lockfile("lockheld");
    let clock = ManualClock::new(20_000);
    let foreign = AdvisoryLock::with_clock(&path, "cron-summary", DEFAULT_TTL_SECS, clock.clone());
    let _held = match foreign.try_acquire() {
        LockOutcome::Acquired(h) => h,
        o => panic!("the foreign writer should acquire, got {o:?}"),
    };

    let tui_lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, clock);
    let mut store = store_over(InMemorySheets::new(), &tui_lock);

    let outcome = submit_ledger_through_store(&mut store, &tui_lock, &a_buy());
    match outcome {
        SubmitOutcome::LockHeld { holder } => {
            assert_eq!(
                holder, "cron-summary",
                "the held outcome names the foreign holder"
            );
        }
        other => panic!("expected LockHeld, got {other:?}"),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        0,
        "a lock-held submit writes nothing (no queue)"
    );
    let _ = std::fs::remove_file(&path);
}

// @spec TUI-ENTRY-FLOW-004
#[test]
fn an_unreachable_workbook_returns_control_with_retry() {
    // The workbook is unreachable: the refreshed-view load fails, so the submit returns
    // control non-destructively (WriteFailed::Unreachable) — the entry is preserved and
    // `[r]etry` is offered. (TUI-ENTRY-FLOW-004)
    let path = temp_lockfile("unreachable");
    let lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, ManualClock::new(10_000));
    let mut sheets = InMemorySheets::new();
    sheets.set_unreachable(true);
    let mut store = store_over(sheets, &lock);

    let outcome = submit_ledger_through_store(&mut store, &lock, &a_buy());
    assert_eq!(
        outcome,
        SubmitOutcome::WriteFailed(WriteFailure::Unreachable),
        "an unreachable workbook returns control with retry"
    );
    let _ = std::fs::remove_file(&path);
}

// @spec TUI-ENTRY-FLOW-010, RUNTIME-EVENTID-001, STORE-WRITE-003
#[test]
fn a_byte_identical_retry_is_idempotent_landing_exactly_once() {
    // The composer freezes the event content at the first submit; a `[r]etry`
    // re-submits the byte-identical event. The id is content-addressed (RUNTIME-EVENTID-001
    // over store's content hash), so the retry assigns the SAME id and store idempotency
    // (STORE-WRITE-003) recognises it as the same event — it lands exactly once.
    // (TUI-ENTRY-FLOW-010)
    let path = temp_lockfile("idempotent");
    let lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, ManualClock::new(10_000));
    let mut store = store_over(InMemorySheets::new(), &lock);

    // The FROZEN event (same value passed to both attempts — never re-derived).
    let frozen = a_buy();

    let first = submit_ledger_through_store(&mut store, &lock, &frozen);
    let first_id = match first {
        SubmitOutcome::Confirmed(o) => {
            assert!(!o.idempotent_skip, "the first submit is a real append");
            o.event_id
        }
        other => panic!("expected Confirmed on the first submit, got {other:?}"),
    };

    // The byte-identical retry: same id, recognised as the same event (idempotent skip).
    let retry = submit_ledger_through_store(&mut store, &lock, &frozen);
    match retry {
        SubmitOutcome::Confirmed(o) => {
            assert_eq!(
                o.event_id, first_id,
                "the retry assigns the SAME content-hash EventId"
            );
            assert!(
                o.idempotent_skip,
                "the byte-identical retry is an idempotent skip"
            );
        }
        other => panic!("expected an idempotent Confirmed on retry, got {other:?}"),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Ledger).unwrap().len(),
        1,
        "the byte-identical retry landed exactly once (idempotent)"
    );
    let _ = std::fs::remove_file(&path);
}

// @spec TUI-ENTRY-FLOW-003, RUNTIME-EVENTID-001
#[test]
fn a_tax_event_submits_confirmed_through_the_same_path() {
    // The tax write path mirrors the ledger one: re-validate against the refreshed
    // realized gains + tax lifecycle, acquire the lock, append + read-back-verify. A
    // valid Allocate on a realized accrual confirms durably; its id is content-addressed
    // (prefix-disjoint from ledger ids). (TUI-ENTRY-FLOW-003, RUNTIME-EVENTID-001)
    let path = temp_lockfile("tax-confirm");
    let lock = AdvisoryLock::with_clock(&path, "tui-1", DEFAULT_TTL_SECS, ManualClock::new(10_000));
    let ctx = context_for_year(2024);

    // Build a workbook with a realized gain: Buy 10 AMZN, then Sell all 10 (a year
    // later, so it realizes). Append both through the store so the live log holds them.
    let mut store = store_over(InMemorySheets::new(), &lock);
    let buy = a_buy(); // L1, AMZN, 10 @ $100 on day 20_000
    assert!(matches!(
        submit_ledger_through_store(&mut store, &lock, &buy),
        SubmitOutcome::Confirmed(_)
    ));
    let sell = LedgerEvent {
        id: String::new(),
        seq: Seq(0),
        date: Date(20_400),
        kind: LedgerEventKind::Sell {
            sale_id: "S1".to_string(),
            symbol: "AMZN".to_string(),
            qty: ms(10),
            unit_price_cents: Cents(15_000),
            fees_cents: Cents(0),
            lot_refs: vec![LotRef {
                lot_id: "L1".to_string(),
                qty: ms(10),
            }],
            accrues_to_state: Some("DC".to_string()),
            platform: "Robinhood".to_string(),
            tracking_code: None,
        },
    };
    assert!(matches!(
        submit_ledger_through_store(&mut store, &lock, &sell),
        SubmitOutcome::Confirmed(_)
    ));

    // Find the realized accrual the Sell produced, and Allocate it (valid on the empty
    // tax log).
    let logs = store.load().unwrap();
    let snapshot = ledger_core::replay(&logs.ledger, &ledger_core::Marks::new());
    let accruals = tax::compute_accruals(&snapshot.realized_gains, &logs.tax, &ctx);
    let key = accruals
        .first()
        .expect("the Sell realized an accrual")
        .key
        .clone();

    let allocate = TaxEvent {
        seq: Seq(0),
        kind: tax::TaxEventKind::Allocate {
            accrual_key: key,
            account_label: "Reserve-Fed".to_string(),
        },
    };
    let outcome = submit_tax_through_store(&mut store, &lock, &ctx, &allocate);
    match outcome {
        SubmitOutcome::Confirmed(o) => {
            assert!(
                o.event_id.starts_with("tax-"),
                "a tax id is prefix-disjoint from ledger ids"
            );
            assert!(!o.idempotent_skip);
        }
        other => panic!("expected a Confirmed tax submit, got {other:?}"),
    }
    assert_eq!(
        store.sheets().read_rows(Tab::Tax).unwrap().len(),
        1,
        "the tax event landed durably"
    );
    let _ = std::fs::remove_file(&path);
}
