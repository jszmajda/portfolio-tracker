//! RED-phase TDD tests for the rebuildable local cache and the two-tier
//! currency probe (STORE-CACHE-001..005).
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use store::cache::{Cache, Fingerprint, InMemoryCache};
use store::serde_rows::ledger_to_row;
use store::sheets::NoopLock;
use store::testkit::InMemorySheets;
use store::{Store, Tab};

// ---------------------------------------------------------------------------
// STORE-CACHE-001: a rebuildable local mirror of BOTH logs, carrying no truth.
// ---------------------------------------------------------------------------

// @spec STORE-CACHE-001
#[test]
fn load_builds_a_local_mirror_of_both_event_logs() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    sheets.push_raw(Tab::Ledger, ledger_to_row(&sell(2, "s2")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let logs = store.load().expect("load");
    let cached = store.cache().read().expect("cache mirrors the logs");
    assert_eq!(cached, logs, "the cache is a mirror of the loaded logs");
    assert_eq!(cached.ledger.len(), 2);
}

// @spec STORE-CACHE-001
#[test]
fn the_cache_carries_no_truth_and_can_be_rebuilt() {
    // A fresh cache rebuilt from the workbook always tracks the workbook; the
    // cache is never the source of truth.
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "only")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load");
    let first = store.cache().read().unwrap();

    // Rebuild from scratch yields the same content (rebuildable / disposable).
    store.load().expect("re-load rebuilds");
    let second = store.cache().read().unwrap();
    assert_eq!(first, second);
}

// ---------------------------------------------------------------------------
// STORE-CACHE-002: the cheap fingerprint probe catches appends, deletions, and
// in-place edits to covered columns; missing/errored cells are treated as a
// change.
// ---------------------------------------------------------------------------

// @spec STORE-CACHE-002
#[test]
fn probe_catches_an_append() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load primes the cache + fingerprint");

    // An append shifts MAX(Seq) and the count → the probe must see a change.
    store
        .sheets()
        .push_raw(Tab::Ledger, ledger_to_row(&buy(2, "b2")));
    assert!(
        store.cache_is_stale(Tab::Ledger).expect("probe"),
        "an append must be detected by the cheap probe"
    );
}

// @spec STORE-CACHE-002
#[test]
fn probe_catches_a_deletion() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(2, "b2")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load");

    // A human deletes a row: the count moves → detected.
    store
        .sheets()
        .set_rows(Tab::Ledger, vec![ledger_to_row(&buy(1, "b1"))]);
    assert!(store.cache_is_stale(Tab::Ledger).expect("probe"));
}

// @spec STORE-CACHE-002
#[test]
fn probe_catches_an_in_place_edit_of_a_covered_column() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load");

    // An in-place edit to a covered cell (the price) shifts the checksum without
    // changing count or MAX(Seq) → still detected.
    let mut edited = ledger_to_row(&buy(1, "b1"));
    edited.set("UnitPriceCents", "999999");
    store.sheets().set_rows(Tab::Ledger, vec![edited]);
    assert!(store.cache_is_stale(Tab::Ledger).expect("probe"));
}

// @spec STORE-CACHE-002
#[test]
fn probe_reports_current_when_nothing_changed() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load");
    assert!(
        !store.cache_is_stale(Tab::Ledger).expect("probe"),
        "an unchanged tab reads as current (the cache keeps its value)"
    );
}

// @spec STORE-CACHE-002
#[test]
fn missing_fingerprint_cells_are_treated_as_a_change() {
    let mut sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    sheets.set_fingerprint_broken(true);
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    // Even a freshly-loaded cache reads as stale when the fingerprint cells are
    // missing/errored.
    let _ = store.load();
    assert!(
        store.cache_is_stale(Tab::Ledger).expect("probe"),
        "missing/errored fingerprint cells force a full read"
    );
}

// ---------------------------------------------------------------------------
// STORE-CACHE-003: the content hash is computed only during a full read; it is
// deterministic over each row's Seq/EventId/field cells.
// ---------------------------------------------------------------------------

// @spec STORE-CACHE-003
#[test]
fn content_hash_is_deterministic_and_change_sensitive() {
    let a = vec![buy(1, "b1"), sell(2, "s2")];
    let b = vec![buy(1, "b1"), sell(2, "s2")];
    assert_eq!(
        Fingerprint::content_hash_ledger(&a),
        Fingerprint::content_hash_ledger(&b),
        "the content hash is deterministic over identical logs"
    );
    // A changed field cell yields a different hash (covers fields beyond the
    // cheap checksum's reach).
    let mut c = a.clone();
    c[0] = vest(1, "b1");
    assert_ne!(
        Fingerprint::content_hash_ledger(&a),
        Fingerprint::content_hash_ledger(&c)
    );
}

// @spec STORE-CACHE-003
#[test]
fn full_read_stores_the_content_hash_in_the_cache_fingerprint() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load is a full read");
    let fp = store
        .cache()
        .ledger_fingerprint()
        .expect("fingerprint stored");
    let logs = store.cache().read().unwrap();
    assert_eq!(
        fp.content_hash,
        Fingerprint::content_hash_ledger(&logs.ledger),
        "the full read stored the content hash"
    );
}

// ---------------------------------------------------------------------------
// STORE-CACHE-004: a probe change or hash mismatch rebuilds the cache from the
// workbook (the workbook wins).
// ---------------------------------------------------------------------------

// @spec STORE-CACHE-004
#[test]
fn probe_change_rebuilds_the_cache_from_the_workbook() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load");

    // Out-of-band append, then a currency-confirming refresh: the workbook wins.
    store
        .sheets()
        .push_raw(Tab::Ledger, ledger_to_row(&sell(2, "s2")));
    let refreshed = store.refresh().expect("refresh rebuilds on a probe change");
    assert_eq!(refreshed.ledger.len(), 2);
    let cached = store.cache().read().unwrap();
    assert!(cached.ledger.iter().any(|e| e.id == "s2"), "workbook wins");
}

// @spec STORE-CACHE-004, STORE-LOAD-006
#[test]
fn on_demand_integrity_verify_catches_an_edit_outside_the_cheap_checksum() {
    // The cheap probe covers only Seq/Date/money + key text columns. An out-of-band
    // edit to a field OUTSIDE that reach (here a Buy's TrackingCode) leaves the
    // cheap probe unchanged, so the hot path would silently serve the stale cache.
    // The on-demand content-hash verification is the belt-and-suspenders that
    // catches it and rebuilds from the workbook. (STORE-CACHE-003/004)
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load primes the cache + content hash");

    // Edit a non-checksum-covered cell out of band. The CHEAP probe must NOT see it.
    let mut edited = ledger_to_row(&buy(1, "b1"));
    edited.set("TrackingCode", "TAMPERED");
    store.sheets().set_rows(Tab::Ledger, vec![edited]);
    assert!(
        !store.cache_is_stale(Tab::Ledger).expect("probe"),
        "the cheap probe does NOT cover TrackingCode — its blind spot"
    );

    // The on-demand content-hash verification DOES catch it and rebuilds.
    let drifted = store.verify_integrity().expect("integrity verify");
    assert!(
        drifted,
        "the content hash caught the out-of-band edit and rebuilt"
    );
    let cached = store.cache().read().unwrap();
    assert_eq!(
        cached.ledger[0].kind,
        store::serde_rows::row_to_ledger(&store.sheets().rows(Tab::Ledger)[0])
            .unwrap()
            .kind,
        "the cache was rebuilt to match the workbook (the workbook wins)"
    );
}

// @spec STORE-CACHE-004
#[test]
fn integrity_verify_reports_no_drift_when_content_matches() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load");
    assert!(
        !store.verify_integrity().expect("verify"),
        "an unedited workbook shows no content drift"
    );
}

// @spec STORE-CACHE-003
#[test]
fn content_hash_covers_an_out_of_band_tax_event_id_edit() {
    // The tax content hash folds each row's real EventId, so an out-of-band edit to
    // a Tax row's EventId cell shifts the hash (STORE-CACHE-003 — the hash is over
    // each row's Seq, EventId, and field cells, including the EventId on the tax tab).
    let a = vec![
        (allocate(1), "tx-1".to_string()),
        (pay(2), "tx-2".to_string()),
    ];
    let mut b = a.clone();
    b[0].1 = "tx-TAMPERED".to_string(); // only the EventId cell changed
    assert_ne!(
        Fingerprint::content_hash_tax(&a),
        Fingerprint::content_hash_tax(&b),
        "a tax EventId edit must shift the content hash"
    );
}

// ---------------------------------------------------------------------------
// STORE-CACHE-005: offline reads are served from the cache.
// ---------------------------------------------------------------------------

// @spec STORE-CACHE-005
#[test]
fn offline_reads_are_served_from_the_cache() {
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "b1")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.load().expect("load while online primes the cache");

    // Go offline: the workbook is unreachable, but cached reads still serve.
    store.sheets_mut().set_unreachable(true);
    let cached = store.read_cached().expect("offline read from the cache");
    assert_eq!(cached.ledger.len(), 1);
    assert_eq!(cached.ledger[0].id, "b1");
}
