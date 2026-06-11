//! RED-phase TDD tests for STORE-SCHEMA-002 (dense per-tab Seq, independent
//! between tabs) and STORE-SCHEMA-003 (globally-unique retry-stable EventId).
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::collections::BTreeSet;

use common::*;
use store::cache::InMemoryCache;
use store::serde_rows::{ledger_to_row, tax_to_row};
use store::sheets::NoopLock;
use store::testkit::InMemorySheets;
use store::{assign_event_id, Store, StoreError, Tab};

// ---------------------------------------------------------------------------
// STORE-SCHEMA-002: dense, contiguous per-tab Seq (1,2,3,…), independent tabs.
// ---------------------------------------------------------------------------

// @spec STORE-SCHEMA-002
#[test]
fn the_two_tabs_have_independent_seq_sequences() {
    // Appending to one tab does not advance the other's Seq; both start at 1.
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let l1 = store.append_ledger(&buy(0, "L1")).expect("ledger append");
    let t1 = store.append_tax(&allocate(0)).expect("tax append");
    let l2 = store.append_ledger(&vest(0, "L2")).expect("ledger append");

    assert_eq!(l1.seq.0, 1, "ledger Seq starts at 1");
    assert_eq!(t1.seq.0, 1, "tax Seq starts at 1, independent of ledger");
    assert_eq!(l2.seq.0, 2, "ledger Seq is contiguous and per-tab");
}

// @spec STORE-SCHEMA-002
#[test]
fn appended_rows_carry_dense_contiguous_seq() {
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    for i in 0..5 {
        let out = store
            .append_ledger(&buy(0, &format!("L{i}")))
            .expect("append");
        assert_eq!(out.seq.0, (i + 1) as u64, "Seq is dense and contiguous");
    }
    // The workbook rows reflect 1..=5 in the Seq column.
    let rows = store.sheets().rows(Tab::Ledger);
    let seqs: Vec<&str> = rows.iter().map(|r| r.get("Seq")).collect();
    assert_eq!(seqs, vec!["1", "2", "3", "4", "5"]);
}

// ---------------------------------------------------------------------------
// STORE-SCHEMA-003: globally-unique EventId across both tabs, invariant across
// retries, used as the idempotency key and the Reversal target.
// ---------------------------------------------------------------------------

// @spec STORE-SCHEMA-003
#[test]
fn assigned_event_ids_are_globally_unique() {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for nonce in 0..1000 {
        let id = assign_event_id(&seen, nonce);
        assert!(!seen.contains(&id), "every assigned EventId is fresh: {id}");
        seen.insert(id);
    }
}

// @spec STORE-SCHEMA-003
#[test]
fn assigned_event_id_avoids_existing_ids_across_both_tabs() {
    // Seed the "existing" set with ids that could be in either tab; the assigner
    // must not collide with any of them (global uniqueness across tabs).
    let mut existing: BTreeSet<String> = BTreeSet::new();
    existing.insert("tx-7".to_string());
    existing.insert("ev-7".to_string());
    let id = assign_event_id(&existing, 7);
    assert!(!existing.contains(&id));
}

// @spec STORE-SCHEMA-003
#[test]
fn event_id_lookup_is_global_a_ledger_id_colliding_with_a_tax_row_is_detected() {
    // EventId is globally unique across BOTH tabs. A live tax append assigns its
    // id; a subsequent ledger append carrying THAT id (a cross-tab collision) must
    // be detected — the row in the OTHER tab cannot deserialize as the ledger
    // event, so it is a WriteVerifyMismatch, never a double-append in a blind spot.
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let tax_out = store
        .append_tax(&allocate(0))
        .expect("tax append assigns an id");
    let tax_id = tax_out.event_id.clone();

    // A ledger event whose id collides with the existing TAX row id.
    let colliding = buy(0, &tax_id);
    let err = store.append_ledger(&colliding).unwrap_err();
    assert_eq!(
        err,
        StoreError::WriteVerifyMismatch,
        "a cross-tab EventId collision is detected (global lookup)"
    );
    // No spurious ledger row landed.
    assert_eq!(store.sheets().rows(Tab::Ledger).len(), 0);
}

// @spec STORE-SCHEMA-003
#[test]
fn event_id_lookup_is_global_a_tax_id_colliding_with_a_ledger_row_is_detected() {
    // The mirror: a tax append whose store-assigned id happens to already exist on
    // the LEDGER tab is a mismatch (the ledger row is not this tax event).
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    // First learn the id append_tax will assign, then seed a colliding ledger row.
    let probe = store.append_tax(&move_(0)).expect("first tax append");
    let tax_id = probe.event_id.clone();

    // Fresh store with a ledger row already carrying that id.
    let sheets2 = InMemorySheets::new();
    sheets2.push_raw(Tab::Ledger, ledger_to_row(&buy(1, &tax_id)));
    let mut store2 = Store::new(sheets2, NoopLock::new(), InMemoryCache::new());
    let err = store2.append_tax(&move_(0)).unwrap_err();
    assert_eq!(
        err,
        StoreError::WriteVerifyMismatch,
        "a tax id colliding with a ledger row is detected (global lookup)"
    );
}

// @spec STORE-SCHEMA-003, STORE-SCHEMA-004
#[test]
fn tax_event_id_is_a_stable_golden_value() {
    // The persisted tax EventId is content-addressed with a FIXED, platform-stable
    // hash (FNV-1a), NOT DefaultHasher — so a retry after a toolchain upgrade
    // computes the SAME id and stays idempotent. This golden value pins the
    // derivation: any future change to the algorithm fails loudly here.
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    let out = store.append_tax(&allocate(0)).expect("tax append");
    assert_eq!(
        out.event_id, "tax-37bb451e6a449c7f",
        "the tax EventId derivation must be stable (golden value)"
    );
}

// @spec STORE-SCHEMA-003, STORE-WRITE-008
#[test]
fn tax_append_is_idempotent_on_the_content_addressed_id() {
    // Two byte-identical tax events derive the same id, so the second append is a
    // retry-stable idempotent skip (no second row). This pins the content-dedup
    // semantics the id derivation bakes in. (Reported as an EARS gap separately.)
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    let first = store.append_tax(&pay(0)).expect("first tax append");
    let retry = store
        .append_tax(&pay(0))
        .expect("retry of the same tax event");
    assert!(!first.idempotent_skip);
    assert!(
        retry.idempotent_skip,
        "byte-identical tax event is an idempotent skip"
    );
    assert_eq!(store.sheets().rows(Tab::Tax).len(), 1, "no second tax row");
    assert_eq!(first.event_id, retry.event_id, "the id is retry-stable");
}

// @spec STORE-WRITE-003
#[test]
fn tax_append_mismatch_when_a_stored_row_with_the_same_id_differs() {
    // A tax row already exists with the id this event would derive, but a field was
    // edited out of band so it differs → WriteVerifyMismatch, no append.
    let mut store_probe = Store::new(InMemorySheets::new(), NoopLock::new(), InMemoryCache::new());
    let id = store_probe
        .append_tax(&move_(0))
        .expect("learn id")
        .event_id;

    let sheets = InMemorySheets::new();
    let mut edited = tax_to_row(&move_(1), &id);
    edited.set("AmountCents", "1"); // a human-edited (differing) stored row
    sheets.push_raw(Tab::Tax, edited);
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let err = store.append_tax(&move_(0)).unwrap_err();
    assert_eq!(err, StoreError::WriteVerifyMismatch);
    assert_eq!(
        store.sheets().rows(Tab::Tax).len(),
        1,
        "the differing row was not appended over"
    );
}

// @spec STORE-SCHEMA-003
#[test]
fn append_idempotency_keys_on_the_event_id_so_retries_are_stable() {
    // An EventId is fixed at creation; re-appending the SAME event (same id) is a
    // retry-stable no-op, never a second row. (The retry-stability of the id is
    // what makes idempotency sound — the write path detail is in write.rs.)
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    let ev = buy(0, "stable-id");
    let first = store.append_ledger(&ev).expect("first append");
    let retry = store
        .append_ledger(&ev)
        .expect("retry with the same EventId");
    assert!(!first.idempotent_skip, "first append lands");
    assert!(
        retry.idempotent_skip,
        "retry with the fixed id is a no-op skip"
    );
    assert_eq!(
        store.sheets().rows(Tab::Ledger).len(),
        1,
        "no double-append"
    );
}
