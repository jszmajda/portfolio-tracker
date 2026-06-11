//! RED-phase TDD tests for replay loading & integrity (STORE-LOAD-001..005):
//! deserialize both tabs for the kernels, refuse a non-dense Seq, refuse an
//! unknown Kind / missing field, enforce the structural Reversal checks, and do
//! NOT gate cross-tab references.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use store::cache::InMemoryCache;
use store::serde_rows::{ledger_to_row, tax_to_row};
use store::sheets::NoopLock;
use store::testkit::InMemorySheets;
use store::{check_ledger_integrity, Row, Store, StoreError, Tab};

// ---------------------------------------------------------------------------
// STORE-LOAD-001: deserialize both tabs into Vec<LedgerEvent> / Vec<TaxEvent>
// and provide them for replay.
// ---------------------------------------------------------------------------

// @spec STORE-LOAD-001
#[test]
fn load_deserializes_both_tabs_for_replay() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    sheets.push_raw(Tab::Ledger, ledger_to_row(&sell(2, "s2")));
    sheets.push_raw(Tab::Tax, tax_to_row(&allocate(1), &"tx-1".to_string()));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let logs = store.load().expect("load");
    assert_eq!(logs.ledger.len(), 2);
    assert_eq!(logs.tax.len(), 1);
    // The folded order is by Seq (the authoritative fold order), not visual order.
    assert_eq!(logs.ledger[0].seq.0, 1);
    assert_eq!(logs.ledger[1].seq.0, 2);
}

// @spec STORE-LOAD-001
#[test]
fn load_orders_by_seq_not_visual_position() {
    // Rows are stored visually out of Seq order (a human sorted the view); the
    // load must fold by the Seq column.
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&sell(2, "s2"))); // Seq 2 first
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1"))); // Seq 1 second
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let logs = store.load().expect("load");
    assert_eq!(logs.ledger[0].seq.0, 1, "folded in Seq order");
    assert_eq!(logs.ledger[1].seq.0, 2);
}

// ---------------------------------------------------------------------------
// STORE-LOAD-002: a non-dense Seq (gap, duplicate, out-of-order) is an integrity
// error; refuse to load rather than fold a corrupt log.
// ---------------------------------------------------------------------------

// @spec STORE-LOAD-002
#[test]
fn a_gap_in_seq_is_a_hard_integrity_error() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(3, "b3"))); // gap: no Seq 2
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    assert_eq!(store.load().unwrap_err(), StoreError::NonDenseSeq);
}

// @spec STORE-LOAD-002
#[test]
fn a_duplicate_seq_is_a_hard_integrity_error() {
    let events = vec![buy(1, "b1"), vest(1, "v1")]; // duplicate Seq 1
    assert_eq!(
        check_ledger_integrity(&events).unwrap_err(),
        StoreError::NonDenseSeq
    );
}

// @spec STORE-LOAD-002
#[test]
fn seq_must_start_at_one() {
    let events = vec![buy(2, "b2"), vest(3, "v3")]; // starts at 2, not 1
    assert_eq!(
        check_ledger_integrity(&events).unwrap_err(),
        StoreError::NonDenseSeq
    );
}

// @spec STORE-LOAD-002
#[test]
fn a_dense_contiguous_seq_passes_integrity() {
    let events = vec![buy(1, "b1"), vest(2, "v2"), split(3, "sp3")];
    assert!(check_ledger_integrity(&events).is_ok());
}

// ---------------------------------------------------------------------------
// STORE-LOAD-003: an unknown Kind or a missing required field is an integrity
// error, never a silent skip.
// ---------------------------------------------------------------------------

// @spec STORE-LOAD-003
#[test]
fn an_unknown_kind_row_fails_the_load() {
    let sheets = InMemorySheets::new();
    let mut bad = Row::new();
    bad.set("Seq", "1");
    bad.set("EventId", "x");
    bad.set("Date", "19000");
    bad.set("Kind", "Wormhole");
    sheets.push_raw(Tab::Ledger, bad);
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    let err = store.load().unwrap_err();
    assert!(matches!(err, StoreError::UnknownKind | StoreError::MissingField));
}

// @spec STORE-LOAD-003
#[test]
fn a_missing_required_field_fails_the_load() {
    let sheets = InMemorySheets::new();
    let mut bad = ledger_to_row(&buy(1, "b1"));
    bad.cells.remove("Symbol"); // a Buy without its required symbol
    sheets.push_raw(Tab::Ledger, bad);
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    assert!(store.load().is_err());
}

// ---------------------------------------------------------------------------
// STORE-LOAD-004: each Reversal's target exists, has a lower Seq, and is not
// itself a Reversal; otherwise an integrity error.
// ---------------------------------------------------------------------------

// @spec STORE-LOAD-004
#[test]
fn a_reversal_targeting_a_lower_seq_non_reversal_passes() {
    let events = vec![buy(1, "b1"), reversal(2, "r2", "b1")];
    assert!(check_ledger_integrity(&events).is_ok());
}

// @spec STORE-LOAD-004
#[test]
fn a_reversal_with_a_missing_target_is_an_integrity_error() {
    let events = vec![buy(1, "b1"), reversal(2, "r2", "ghost")];
    assert_eq!(
        check_ledger_integrity(&events).unwrap_err(),
        StoreError::BadReversal
    );
}

// @spec STORE-LOAD-004
#[test]
fn a_reversal_targeting_a_higher_seq_is_an_integrity_error() {
    // Target b3 has Seq 3, the reversal has Seq 2 → target is not lower.
    let events = vec![buy(1, "b1"), reversal(2, "r2", "b3"), buy(3, "b3")];
    assert_eq!(
        check_ledger_integrity(&events).unwrap_err(),
        StoreError::BadReversal
    );
}

// @spec STORE-LOAD-004
#[test]
fn a_reversal_targeting_another_reversal_is_an_integrity_error() {
    let events = vec![buy(1, "b1"), reversal(2, "r2", "b1"), reversal(3, "r3", "r2")];
    assert_eq!(
        check_ledger_integrity(&events).unwrap_err(),
        StoreError::BadReversal
    );
}

// ---------------------------------------------------------------------------
// STORE-LOAD-005: cross-tab references are NOT store's gate. A Tax Event
// referencing a sale_id/lot_id with no backing RealizedGain must load fine here;
// the tax kernel's orphan rule (TAX-VERIF-007) handles it downstream.
// ---------------------------------------------------------------------------

// @spec STORE-LOAD-005
#[test]
fn store_does_not_gate_cross_tab_references() {
    let sheets = InMemorySheets::new();
    // A ledger log with NO sale backing the tax event's accrual key.
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    // A tax Move/Allocate referencing sale-1/lot-1 which has no RealizedGain.
    sheets.push_raw(Tab::Tax, tax_to_row(&allocate(1), &"tx-1".to_string()));
    sheets.push_raw(Tab::Tax, tax_to_row(&move_(2), &"tx-2".to_string()));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    // store loads both logs without rejecting the orphan tax events.
    let logs = store.load().expect("store does not gate cross-tab refs");
    assert_eq!(logs.tax.len(), 2);
}
