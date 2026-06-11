//! Cache-vs-workbook divergence DETECTION and rebuild semantics:
//! RUNTIME-CACHE-001/002. runtime owns the per-tab currency probe / fingerprint
//! comparison (workbook authoritative) and the choice of full-replace (cache cannot
//! be trusted incrementally) vs reconcile (an append-only tail the fingerprint can
//! localize). store/config consume the rebuilt cache; they do not author the
//! mechanism.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::small_logs;

use runtime::cache::{detect_tab_divergence};
use runtime::{
    classify_divergence, detect_and_rebuild, detect_divergence, Divergence, RebuildStrategy,
};
use store::serde_rows;
use store::sheets::ProbeCells;
use store::testkit::InMemorySheets;
use store::{Cache, Fingerprint, InMemoryCache, NoopLock, Store, Tab};

fn fp(count: i64, max_seq: i64, checksum: i64) -> Fingerprint {
    Fingerprint { count, max_seq, checksum, content_hash: 0 }
}

fn probe(count: i64, max_seq: i64, checksum: i64) -> ProbeCells {
    ProbeCells { count: Some(count), max_seq: Some(max_seq), checksum: Some(checksum) }
}

// @spec RUNTIME-CACHE-001
#[test]
fn an_unchanged_fingerprint_is_in_sync_no_rebuild() {
    // Equal count + max_seq + checksum ⇒ the cache still mirrors the workbook.
    // (RUNTIME-CACHE-001)
    let stored = fp(3, 3, 100);
    let p = probe(3, 3, 100);
    let d = classify_divergence(Some(&stored), &p);
    assert_eq!(d, Divergence::InSync);
    assert_eq!(d.strategy(), RebuildStrategy::None);
}

// @spec RUNTIME-CACHE-002
#[test]
fn an_append_only_tail_reconciles() {
    // BOTH the count and the max-Seq grew (new rows carry higher Seq) and the
    // checksum shifted with them: an append-only extension the fingerprint localizes
    // ⇒ reconcile. (RUNTIME-CACHE-002)
    let stored = fp(3, 3, 100);
    let p = probe(5, 5, 250); // two rows appended.
    let d = classify_divergence(Some(&stored), &p);
    assert_eq!(d, Divergence::AppendedTail);
    assert_eq!(d.strategy(), RebuildStrategy::Reconcile);
}

// @spec RUNTIME-CACHE-002
#[test]
fn an_out_of_band_deletion_forces_full_replace() {
    // The count SHRANK (a row was deleted out-of-band) — not an append-only tail, so
    // the cache cannot be trusted incrementally ⇒ full-replace. (RUNTIME-CACHE-002)
    let stored = fp(5, 5, 250);
    let p = probe(4, 5, 230); // a row deleted; count down, max_seq unchanged.
    let d = classify_divergence(Some(&stored), &p);
    assert_eq!(d, Divergence::Untrustworthy);
    assert_eq!(d.strategy(), RebuildStrategy::FullReplace);
}

// @spec RUNTIME-CACHE-002
#[test]
fn an_in_place_edit_forces_full_replace() {
    // The checksum MOVED but count + max_seq did not both grow (an in-place edit to a
    // covered cell) — not a localizable tail ⇒ full-replace. (RUNTIME-CACHE-002)
    let stored = fp(3, 3, 100);
    let p = probe(3, 3, 175); // same count/seq, different checksum.
    let d = classify_divergence(Some(&stored), &p);
    assert_eq!(d, Divergence::Untrustworthy);
    assert_eq!(d.strategy(), RebuildStrategy::FullReplace);
}

// @spec RUNTIME-CACHE-001, RUNTIME-CACHE-002
#[test]
fn a_missing_or_errored_fingerprint_is_untrustworthy() {
    // A missing/errored probe cell cannot localize a tail; the workbook is
    // authoritative and the cache cannot be trusted incrementally ⇒ full-replace.
    // (RUNTIME-CACHE-001/002)
    let stored = fp(3, 3, 100);
    let errored = ProbeCells { count: None, max_seq: Some(3), checksum: Some(100) };
    assert_eq!(classify_divergence(Some(&stored), &errored), Divergence::Untrustworthy);
}

// @spec RUNTIME-CACHE-002
#[test]
fn a_cold_cache_forces_full_replace() {
    // No stored fingerprint (cold cache, never built) ⇒ full-replace from a complete
    // workbook read. (RUNTIME-CACHE-002)
    let p = probe(3, 3, 100);
    let d = classify_divergence(None, &p);
    assert_eq!(d, Divergence::Untrustworthy);
    assert_eq!(d.strategy(), RebuildStrategy::FullReplace);
}

// @spec RUNTIME-CACHE-001
#[test]
fn detection_runs_the_store_probe_against_the_cache_fingerprint() {
    // runtime's detection drives store's cheap probe and compares it against the
    // cache's stored fingerprint — the workbook is authoritative. A cold cache reads
    // as untrustworthy (forces a full read). (RUNTIME-CACHE-001)
    let logs = small_logs();
    let ledger_rows: Vec<_> = logs.ledger.iter().map(serde_rows::ledger_to_row).collect();
    let sheets = InMemorySheets::seeded(ledger_rows, vec![]);
    let store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    // A cold cache (never built) is untrustworthy on detection.
    assert_eq!(
        detect_tab_divergence(&store, Tab::Ledger).expect("detect"),
        Divergence::Untrustworthy,
        "a cold cache forces a full read"
    );
    assert_eq!(detect_divergence(&store).expect("detect both"), Divergence::Untrustworthy);
}

// @spec RUNTIME-CACHE-001, RUNTIME-CACHE-002
#[test]
fn detect_and_rebuild_full_replaces_a_cold_cache_then_reads_in_sync() {
    // End-to-end: a cold cache detects untrustworthy and detect_and_rebuild
    // full-replaces from the authoritative workbook (store's full read). After the
    // rebuild the cache is populated and a second detection reads in-sync (the
    // workbook won, the fingerprint now matches). (RUNTIME-CACHE-001/002)
    let logs = small_logs();
    let ledger_rows: Vec<_> = logs.ledger.iter().map(serde_rows::ledger_to_row).collect();
    let sheets = InMemorySheets::seeded(ledger_rows, vec![]);
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    // First detect_and_rebuild: cold cache ⇒ FullReplace from the workbook.
    let strategy = detect_and_rebuild(&mut store).expect("detect + rebuild");
    assert_eq!(strategy, RebuildStrategy::FullReplace, "a cold cache is rebuilt full-replace");
    assert!(store.cache().is_populated(), "the cache was materialized from the workbook");

    // The rebuilt cache mirrors the authoritative log.
    let cached = store.cache().read().expect("read the rebuilt cache");
    assert_eq!(cached.ledger.len(), logs.ledger.len(), "the workbook log was materialized");

    // A second detection now reads in-sync — nothing diverged after the rebuild.
    assert_eq!(detect_divergence(&store).expect("re-detect"), Divergence::InSync);
    let strategy2 = detect_and_rebuild(&mut store).expect("re-detect + rebuild");
    assert_eq!(strategy2, RebuildStrategy::None, "an in-sync cache is not rebuilt again");
}
