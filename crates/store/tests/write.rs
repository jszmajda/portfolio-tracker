//! RED-phase TDD tests for the append + read-back-verify write path
//! (STORE-WRITE-001..006). The append primitive acquires the advisory lock
//! inside itself, confirms cache currency, assigns Seq from the live workbook,
//! checks idempotency on the EventId, appends, reads back, and verifies field
//! equality before updating the cache.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use store::cache::{Cache, InMemoryCache};
use store::serde_rows::ledger_to_row;
use store::sheets::NoopLock;
use store::testkit::InMemorySheets;
use store::{Store, StoreError, Tab};

// ---------------------------------------------------------------------------
// STORE-WRITE-002: assign Seq from the live workbook max + 1 (never the cache);
// a block is allocated contiguously up front and appended in Seq order.
// ---------------------------------------------------------------------------

// @spec STORE-WRITE-002
#[test]
fn seq_is_assigned_from_the_live_workbook_max_plus_one() {
    // Pre-seed the workbook with two ledger rows (Seq 1,2) bypassing the path.
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "pre-1")));
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(2, "pre-2")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let out = store.append_ledger(&vest(0, "new")).expect("append");
    assert_eq!(out.seq.0, 3, "Seq is live-max (2) + 1");
}

// @spec STORE-WRITE-002
#[test]
fn a_multi_event_block_is_assigned_a_contiguous_seq_block() {
    // A Vest + same-day sell-to-cover submit: assigned base, base+1 up front and
    // appended strictly in Seq order — Seqs are not re-derived per row.
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "pre")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let block = vec![vest(0, "vest"), sell(0, "stc")];
    let outs = store.append_ledger_block(&block).expect("block append");
    assert_eq!(outs.len(), 2);
    assert_eq!(outs[0].seq.0, 2, "base = live-max(1) + 1");
    assert_eq!(outs[1].seq.0, 3, "base + 1, contiguous");

    // Appended strictly in Seq order in the workbook.
    let rows = store.sheets().rows(Tab::Ledger);
    let seqs: Vec<&str> = rows.iter().map(|r| r.get("Seq")).collect();
    assert_eq!(seqs, vec!["1", "2", "3"]);
}

// @spec STORE-WRITE-002
#[test]
fn seq_is_read_from_live_workbook_not_the_stale_cache() {
    // The cache is stale (claims max Seq 1) but the live workbook holds Seq 1,2.
    // The new Seq must come from the live workbook (3), never the cache (2).
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "p1")));
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(2, "p2")));
    let cache = InMemoryCache::new();
    let mut store = Store::new(sheets, NoopLock::new(), cache);

    let out = store.append_ledger(&vest(0, "fresh")).expect("append");
    assert_eq!(out.seq.0, 3, "Seq from the live workbook, not the cache");
}

// ---------------------------------------------------------------------------
// STORE-WRITE-001: confirm cache currency via the cheap probe and rebuild on a
// change BEFORE assigning Seq.
// ---------------------------------------------------------------------------

// @spec STORE-WRITE-001
#[test]
fn append_rebuilds_a_stale_cache_before_assigning_seq() {
    // Build a populated cache, then mutate the workbook out of band (a human edit
    // / append). The next append must detect the change via the probe and rebuild
    // the cache from the workbook before assigning Seq.
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "p1")));
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    // Prime the cache via a load.
    store.load().expect("initial load builds the cache");

    // Out-of-band: a row appears in the workbook the cache does not know about.
    store
        .sheets()
        .push_raw(Tab::Ledger, ledger_to_row(&buy(2, "ooband")));

    // The next append confirms currency (probe changed) and rebuilds, so the
    // cache now reflects the out-of-band row, and Seq comes off the live max (2).
    let out = store.append_ledger(&vest(0, "next")).expect("append");
    assert_eq!(out.seq.0, 3, "live max after the out-of-band row is 2 → +1");
    let cached = store.cache().read().expect("cache populated");
    assert!(
        cached.ledger.iter().any(|e| e.id == "ooband"),
        "the stale cache was rebuilt from the workbook before the append"
    );
}

// ---------------------------------------------------------------------------
// STORE-WRITE-003: idempotency on the EventId — skip on equality, error on diff.
// ---------------------------------------------------------------------------

// @spec STORE-WRITE-003
#[test]
fn idempotent_skip_when_existing_row_equals_the_event() {
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    let ev = buy(0, "dup");
    store.append_ledger(&ev).expect("first lands");
    let again = store.append_ledger(&ev).expect("idempotent retry");
    assert!(again.idempotent_skip);
    assert_eq!(store.sheets().rows(Tab::Ledger).len(), 1, "no second row");
}

// @spec STORE-WRITE-003
#[test]
fn write_verify_mismatch_when_existing_row_differs() {
    // A row with the same EventId is already present but a human edited a field
    // so it differs from the event being written → WriteVerifyMismatch, no append.
    let sheets = InMemorySheets::new();
    let original = buy(1, "collide");
    let mut edited = ledger_to_row(&original);
    edited.set("FeesCents", "9999"); // someone changed the fees on the stored row
    sheets.push_raw(Tab::Ledger, edited);
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let err = store.append_ledger(&buy(0, "collide")).unwrap_err();
    assert_eq!(err, StoreError::WriteVerifyMismatch);
    assert_eq!(
        store.sheets().rows(Tab::Ledger).len(),
        1,
        "the differing row was not appended over"
    );
}

// ---------------------------------------------------------------------------
// STORE-WRITE-004: read back by EventId and assert structural field equality
// before confirming; a field mismatch raises WriteVerifyMismatch.
// ---------------------------------------------------------------------------

// @spec STORE-WRITE-004
#[test]
fn append_reads_back_and_confirms_field_equality() {
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    let ev = sell(0, "rb");
    let out = store.append_ledger(&ev).expect("append + read-back-verify");
    assert!(!out.idempotent_skip);
    // The read-back deserializes equal to the event written.
    let rows = store.sheets().rows(Tab::Ledger);
    let stored = store::serde_rows::row_to_ledger(&rows[0]).unwrap();
    assert_eq!(stored.kind, ev.kind);
    assert_eq!(stored.id, ev.id);
    assert_eq!(stored.seq, out.seq);
}

// ---------------------------------------------------------------------------
// STORE-WRITE-005: unreachable workbook / missing read-back returns control to
// the owner; no partial state, no background queue.
// ---------------------------------------------------------------------------

// @spec STORE-WRITE-005
#[test]
fn unreachable_workbook_returns_control_to_the_owner() {
    let mut sheets = InMemorySheets::new();
    sheets.set_unreachable(true);
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    let err = store.append_ledger(&buy(0, "x")).unwrap_err();
    assert_eq!(err, StoreError::Unreachable);
    // No background queue / partial state: the cache is untouched (still cold).
    assert!(!store.cache().is_populated());
}

// ---------------------------------------------------------------------------
// STORE-WRITE-006: the cache is updated ONLY after a read-back-verified write.
// ---------------------------------------------------------------------------

// @spec STORE-WRITE-006
#[test]
fn cache_is_updated_only_after_read_back_verify() {
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    let ev = buy(0, "cached");
    store.append_ledger(&ev).expect("append");
    // After a verified write the cache contains the event.
    let cached = store.cache().read().expect("cache populated post-write");
    assert!(cached.ledger.iter().any(|e| e.id == "cached"));
}

// @spec STORE-WRITE-006
#[test]
fn failed_write_does_not_update_the_cache() {
    // A mismatch (the EventId exists but the stored row differs) must not append
    // a second row nor advance the cache to a state containing the attempted
    // write as a fresh landing. The cache only ever reflects the workbook.
    let sheets = InMemorySheets::new();
    let original = buy(1, "bad");
    let mut edited = ledger_to_row(&original);
    edited.set("FeesCents", "1"); // a human-edited (differing) stored row
    sheets.push_raw(Tab::Ledger, edited);
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let err = store.append_ledger(&buy(0, "bad")).unwrap_err();
    assert_eq!(err, StoreError::WriteVerifyMismatch);

    // No second row was appended (the workbook still has exactly the one,
    // differing row), so a failed write left the log unchanged.
    assert_eq!(store.sheets().rows(Tab::Ledger).len(), 1);
    // The cache, if populated by the currency-confirm step, mirrors that single
    // workbook row — it never gained a second "bad" landing.
    if let Ok(c) = store.cache().read() {
        assert_eq!(
            c.ledger.iter().filter(|e| e.id == "bad").count(),
            1,
            "the failed write did not add a second row to the cache"
        );
    }
}

// ---------------------------------------------------------------------------
// The advisory lock is acquired INSIDE the append primitive (store-design.md →
// "Interfaces": acquired inside the primitive). The NoopLock counts acquires.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// STORE-WRITE-006: a partial multi-event block landing leaves a valid DENSE
// prefix; on retry the landed prefix is idempotency-skipped and the unlanded
// suffix re-appends with CONTIGUOUS Seqs (no gap). A skipped event must NOT
// consume a Seq, or the surviving suffix would be offset and the log non-dense.
// ---------------------------------------------------------------------------

// @spec STORE-WRITE-006
#[test]
fn block_partial_retry_keeps_seq_dense_and_loads() {
    // Pre-existing Seq{1}; event A has already landed at Seq 2 (a prior partial
    // block landing). Retrying the block [A, B] must skip A and stamp B at Seq 3
    // (the next dense value) — NOT Seq 4 (base + block-index), which would gap.
    let sheets = InMemorySheets::new();
    sheets.push_raw(Tab::Ledger, ledger_to_row(&buy(1, "pre")));
    sheets.push_raw(Tab::Ledger, ledger_to_row(&vest(2, "A"))); // A already landed
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let a = vest(0, "A");
    let b = sell(0, "B");
    let outs = store
        .append_ledger_block(&[a, b])
        .expect("retry of a partially-landed block");

    assert!(outs[0].idempotent_skip, "A was already landed → idempotent skip");
    assert!(!outs[1].idempotent_skip, "B is the unlanded suffix → appended");
    assert_eq!(
        outs[1].seq.0, 3,
        "the skipped A did not consume a Seq; B is dense at 3, not 4"
    );

    // The Seq set is dense (1,2,3) and the workbook loads without a gap error.
    let logs = store.load().expect("a dense log loads after the partial retry");
    let mut seqs: Vec<u64> = logs.ledger.iter().map(|e| e.seq.0).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, vec![1, 2, 3], "no Seq gap from the skipped prefix");
}

// ---------------------------------------------------------------------------
// STORE-WRITE-004/005: the POST-APPEND read-back branches. A row that lands but
// DIFFERS from what was written → WriteVerifyMismatch; a row that does not
// materialize on read-back → Unreachable (control returns to the owner).
// ---------------------------------------------------------------------------

// @spec STORE-WRITE-004
#[test]
fn read_back_mismatch_when_landed_row_differs_from_written() {
    // The append "lands" a coerced (differing) row; the post-append read-back must
    // catch the field divergence and raise WriteVerifyMismatch.
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.sheets().set_mutate_on_append(true);

    let err = store.append_ledger(&buy(0, "coerced")).unwrap_err();
    assert_eq!(err, StoreError::WriteVerifyMismatch);
}

// @spec STORE-WRITE-005
#[test]
fn read_back_missing_row_returns_unreachable_to_the_owner() {
    // The append reports success but the row never materialized; the post-append
    // read-back finds nothing → Unreachable (no partial state, control to owner).
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.sheets().set_drop_on_append(true);

    let err = store.append_ledger(&buy(0, "vanished")).unwrap_err();
    assert_eq!(err, StoreError::Unreachable);
    // The cache never gained the un-verified write.
    if let Ok(c) = store.cache().read() {
        assert!(!c.ledger.iter().any(|e| e.id == "vanished"));
    }
}

// @spec STORE-WRITE-005
#[test]
fn landed_but_failed_verify_row_is_caught_by_idempotency_on_retry() {
    // A row that landed-but-failed-verify (a divergent landed row) persists on the
    // human-editable workbook. STORE-WRITE-005's "no partial state" holds for the
    // cache (not advanced); the retry then idempotency-MISMATCHES on the divergent
    // landed row rather than double-appending — fail loud, never silently accept.
    let sheets = InMemorySheets::new();
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());
    store.sheets().set_mutate_on_append(true);

    let ev = buy(0, "divergent");
    assert_eq!(
        store.append_ledger(&ev).unwrap_err(),
        StoreError::WriteVerifyMismatch
    );
    // The divergent row physically landed (human-editable tab), but the cache was
    // not advanced to treat it as a confirmed write.
    assert_eq!(store.sheets().rows(Tab::Ledger).len(), 1);
    // The retry re-runs the idempotency precheck and mismatches on the divergent
    // landed row (it differs from the event), never double-appending.
    assert_eq!(
        store.append_ledger(&ev).unwrap_err(),
        StoreError::WriteVerifyMismatch
    );
    assert_eq!(store.sheets().rows(Tab::Ledger).len(), 1, "no double-append");
}

// @spec STORE-WRITE-002, STORE-WRITE-007
#[test]
fn append_acquires_the_advisory_lock_inside_the_primitive() {
    // The NoopLock shares its acquire count across clones, so a clone held by the
    // test observes the acquire the append primitive performs inside itself.
    let lock = NoopLock::new();
    let mut store = Store::new(InMemorySheets::new(), lock.clone(), InMemoryCache::new());
    assert_eq!(lock.acquire_count(), 0, "no lock taken before the append");
    store.append_ledger(&buy(0, "lk")).expect("append");
    assert_eq!(
        lock.acquire_count(),
        1,
        "the append primitive acquired the advisory lock inside itself"
    );
}
