//! Fresh-workbook cold start (STORE-LOAD-007 / STORE-WRITE-009): a missing
//! event-log tab is an EMPTY log, not an outage, and the first append creates
//! the tab. A genuine transport failure stays `Unreachable`.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use store::cache::InMemoryCache;
use store::sheets::NoopLock;
use store::testkit::InMemorySheets;
use store::{Store, StoreError, Tab};

// ---------------------------------------------------------------------------
// STORE-LOAD-007: missing tabs read as an empty event log (cold start).
// ---------------------------------------------------------------------------

// @spec STORE-LOAD-007
#[test]
fn a_fresh_workbook_with_no_event_log_tabs_loads_as_an_empty_book() {
    let sheets = InMemorySheets::fresh_workbook();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let logs = store.load().expect("a fresh workbook is a cold start, not an error");
    assert!(logs.ledger.is_empty(), "no ledger tab yet ⇒ empty ledger log");
    assert!(logs.tax.is_empty(), "no tax tab yet ⇒ empty tax log");
}

// @spec STORE-LOAD-007
#[test]
fn one_missing_tab_reads_empty_while_the_existing_tab_loads() {
    use store::serde_rows::ledger_to_row;
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    sheets.set_tab_missing(Tab::Tax, true); // tax tab never created yet

    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    let logs = store.load().expect("one missing tab is still a cold start");
    assert_eq!(logs.ledger.len(), 1, "the existing tab loads normally");
    assert!(logs.tax.is_empty(), "the missing tab reads as empty");
}

// @spec STORE-LOAD-007
#[test]
fn refresh_on_a_fresh_workbook_serves_the_empty_book() {
    let sheets = InMemorySheets::fresh_workbook();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let logs = store.refresh().expect("refresh cold-starts too");
    assert!(logs.ledger.is_empty() && logs.tax.is_empty());
}

// @spec STORE-LOAD-007
#[test]
fn a_transport_failure_is_still_unreachable_not_a_cold_start() {
    let mut sheets = InMemorySheets::fresh_workbook();
    sheets.set_unreachable(true); // offline wins: tab existence is unknowable

    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    assert_eq!(
        store.load().unwrap_err(),
        StoreError::Unreachable,
        "offline must NOT be misread as an empty book"
    );
}

// ---------------------------------------------------------------------------
// STORE-WRITE-009: the first append creates the missing tab, then retries.
// ---------------------------------------------------------------------------

// @spec STORE-WRITE-009
#[test]
fn first_append_to_a_fresh_workbook_creates_the_tab_and_lands_the_event() {
    let sheets = InMemorySheets::fresh_workbook();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let out = store.append_ledger(&buy(0, "first")).expect("first-ever append bootstraps");
    assert_eq!(out.seq.0, 1, "the first event of a fresh book is Seq 1");
    assert!(
        !store.sheets().tab_missing(Tab::Ledger),
        "the append created the ledger tab"
    );
    assert_eq!(store.sheets().rows(Tab::Ledger).len(), 1, "the event landed");
}

// @spec STORE-WRITE-009
#[test]
fn first_tax_append_to_a_fresh_workbook_bootstraps_the_tax_tab() {
    let sheets = InMemorySheets::fresh_workbook();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    store.append_tax(&allocate(0)).expect("tax append bootstraps its tab");
    assert!(!store.sheets().tab_missing(Tab::Tax), "the append created the tax tab");
    assert_eq!(store.sheets().rows(Tab::Tax).len(), 1);
}

// @spec STORE-WRITE-009
#[test]
fn a_second_append_rides_the_existing_tab_without_recreating() {
    let sheets = InMemorySheets::fresh_workbook();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    store.append_ledger(&buy(0, "first")).expect("bootstrap append");
    let out = store.append_ledger(&vest(0, "second")).expect("normal append");
    assert_eq!(out.seq.0, 2, "Seq continues densely on the bootstrapped tab");
    assert_eq!(store.sheets().rows(Tab::Ledger).len(), 2);
}
