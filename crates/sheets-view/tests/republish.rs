//! Republish: SHEET-PUB-001/002/003.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::collections::BTreeMap;

use sheets_view::testkit::{pass, InMemorySheetsView, SeamCall};
use sheets_view::{
    Cell, PriceReading, Publisher, SettleConfig, ViewTab, POSITIONS_TAB,
};

use pt_core::{Cents, Date, NoopLock};

fn tab(name: &str, rows: Vec<Vec<Cell>>) -> ViewTab {
    ViewTab {
        name: name.to_string(),
        header: vec!["Symbol".to_string(), "Shares".to_string()],
        rows,
    }
}

fn rows(specs: &[(&str, &str)]) -> Vec<Vec<Cell>> {
    specs
        .iter()
        .map(|(a, b)| vec![Cell::Value(a.to_string()), Cell::Value(b.to_string())])
        .collect()
}

// @spec SHEET-PUB-001
#[test]
fn republish_then_settle_is_one_serialized_loop_settle_after_write() {
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[(
        "AMZN",
        PriceReading::Numeric { price_usd: 200.0, quote_date: Date(19_150) },
    )]));
    let mut pub_ = Publisher::new(client);

    let t = tab(POSITIONS_TAB, rows(&[("AMZN", "3")]));
    let (outcome, marks) = pub_
        .republish_then_settle(
            &[t],
            &["AMZN".to_string()],
            &BTreeMap::new(),
            SettleConfig::default(),
            &NoopLock::new(),
            "2022-09-01",
        )
        .unwrap();

    // The publish landed and the settle pass ran AFTER it (one serialized loop).
    assert!(outcome.published);
    assert!(pub_.client().published(POSITIONS_TAB).is_some());
    assert_eq!(marks.marks.get("AMZN").map(|m| m.price_cents), Some(Cents(200_00)));
    // The settle pass read the price cells (a separate pass after the write).
    assert!(pub_.client().read_count() >= 1);
}

// @spec SHEET-MARK-001
#[test]
fn every_settle_read_happens_after_all_writes_in_a_cycle() {
    // The settle read must never be inline with / before the formula write. With a
    // multi-tab republish + a transient→numeric settle (so several reads happen),
    // assert from the recorded seam-call order that EVERY ReadPricePass falls after
    // ALL BatchUpdate writes in the cycle. (SHEET-MARK-001)
    let client = InMemorySheetsView::new();
    // Two polls: transient first, then numeric — forces > 1 settle read.
    client.set_price_script(vec![
        pass(&[("AMZN", PriceReading::Transient)]),
        pass(&[("AMZN", PriceReading::Numeric { price_usd: 200.0, quote_date: Date(19_150) })]),
    ]);
    let mut pub_ = Publisher::new(client);

    // Republish several tabs, then settle.
    let tabs = vec![
        tab(POSITIONS_TAB, rows(&[("AMZN", "3")])),
        tab("Open Lots", rows(&[("AMZN", "3")])),
        tab("Realized", rows(&[("GOOG", "1")])),
    ];
    let (_outcome, marks) = pub_
        .republish_then_settle(
            &tabs,
            &["AMZN".to_string()],
            &BTreeMap::new(),
            SettleConfig::default(),
            &NoopLock::new(),
            "2022-09-01",
        )
        .unwrap();
    assert_eq!(marks.marks.get("AMZN").map(|m| m.price_cents), Some(Cents(200_00)));

    let log = pub_.client().call_log();
    // There are both writes and reads, and more than one read (it polled to settle).
    let writes: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == SeamCall::BatchUpdate)
        .map(|(i, _)| i)
        .collect();
    let reads: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == SeamCall::ReadPricePass)
        .map(|(i, _)| i)
        .collect();
    assert!(!writes.is_empty(), "writes happened");
    assert!(reads.len() >= 2, "polled to settle: {reads:?}");
    let last_write = *writes.iter().max().unwrap();
    let first_read = *reads.iter().min().unwrap();
    assert!(
        first_read > last_write,
        "every settle read must happen AFTER all writes: writes={writes:?} reads={reads:?}"
    );
}

// @spec SHEET-PUB-002
#[test]
fn republish_is_atomic_batchupdate_with_tail_truncate() {
    let mut pub_ = Publisher::new(InMemorySheetsView::new());

    // First publish: 3 rows.
    let t1 = tab(POSITIONS_TAB, rows(&[("AMZN", "3"), ("GOOG", "2"), ("MSFT", "1")]));
    let o1 = pub_.republish(&[t1], &NoopLock::new(), "2022-09-01");
    assert!(o1.published);
    assert_eq!(pub_.client().published(POSITIONS_TAB).unwrap().rows.len(), 3);

    // Republish with FEWER rows (a sold-out symbol): the tail is truncated to the
    // exact new row count — no leftover stale rows, no half-written window.
    let t2 = tab(POSITIONS_TAB, rows(&[("AMZN", "3")]));
    let o2 = pub_.republish(&[t2], &NoopLock::new(), "2022-09-02");
    assert!(o2.published);
    let published = pub_.client().published(POSITIONS_TAB).unwrap();
    assert_eq!(published.rows.len(), 1, "residual tail must be truncated");
    assert_eq!(published.rows[0][0].text(), "AMZN");
}

// @spec SHEET-PUB-003
#[test]
fn failed_republish_marks_stale_writes_banner_and_retries() {
    let client = InMemorySheetsView::new();
    let mut pub_ = Publisher::new(client);

    // Arm a publish failure.
    pub_.client_mut().set_publish_fails(true);
    let t = tab(POSITIONS_TAB, rows(&[("AMZN", "3")]));
    let outcome = pub_.republish(&[t.clone()], &NoopLock::new(), "2022-09-01");

    // The republish failed → the view is flagged stale, a best-effort in-tab
    // banner is written, and nothing landed (the canonical log is store's,
    // unaffected — there is no store interaction here at all).
    assert!(!outcome.published);
    assert!(outcome.stale_tabs.contains(&POSITIONS_TAB.to_string()));
    assert!(pub_.client().published(POSITIONS_TAB).is_none(), "failed publish lands nothing");
    let banner = pub_.client().banner(POSITIONS_TAB).expect("stale banner");
    assert!(banner.contains("STALE"), "banner: {banner}");
    assert!(banner.contains("2022-09-01"), "banner carries the last-sync ts: {banner}");

    // Retry on the next sync: the failure clears and the tab publishes, clearing
    // the stale flag.
    pub_.client_mut().set_publish_fails(false);
    let outcome2 = pub_.republish(&[t], &NoopLock::new(), "2022-09-02");
    assert!(outcome2.published);
    assert!(!outcome2.stale_tabs.contains(&POSITIONS_TAB.to_string()));
    assert!(pub_.client().published(POSITIONS_TAB).is_some());
}

// @spec SHEET-PUB-003
#[test]
fn banner_write_failure_leaves_tui_as_staleness_surface() {
    let client = InMemorySheetsView::new();
    let mut pub_ = Publisher::new(client);

    // Both the publish AND the best-effort banner fail (network fully down).
    pub_.client_mut().set_publish_fails(true);
    pub_.client_mut().set_banner_fails(true);
    let t = tab(POSITIONS_TAB, rows(&[("AMZN", "3")]));
    let outcome = pub_.republish(&[t], &NoopLock::new(), "2022-09-01");

    // The tab is still recorded stale (the TUI remains the staleness surface),
    // and no banner was written.
    assert!(!outcome.published);
    assert!(outcome.stale_tabs.contains(&POSITIONS_TAB.to_string()));
    assert!(pub_.client().banner(POSITIONS_TAB).is_none());
    // The stale flag persists across the cycle for the TUI to surface.
    assert!(pub_.stale_tabs().contains(&POSITIONS_TAB.to_string()));
}

/// A `Lock` whose `acquire` always reports the lock is held by another holder, so
/// `republish` must NOT write concurrently — it returns `published: false` and the
/// workbook is left untouched. Mirrors `StoreLockAdapter`'s `Held -> LockError::Held`.
struct HeldLock;

impl pt_core::Lock for HeldLock {
    fn acquire(&self) -> Result<pt_core::LockGuard, pt_core::LockError> {
        Err(pt_core::LockError::Held)
    }
}

// @spec SHEET-PUB-004
#[test]
fn republish_acquires_the_lock_before_mutating() {
    // A successful republish through a free (acquire-counting) lock acquires the
    // advisory write-lock exactly once around the mutation. (SHEET-PUB-004)
    let mut pub_ = Publisher::new(InMemorySheetsView::new());
    let lock = NoopLock::new();

    let t = tab(POSITIONS_TAB, rows(&[("AMZN", "3")]));
    let outcome = pub_.republish(&[t], &lock, "2022-09-01");

    assert!(outcome.published);
    assert_eq!(lock.acquire_count(), 1, "republish acquires the advisory lock once");
    assert!(pub_.client().published(POSITIONS_TAB).is_some());
}

// @spec SHEET-PUB-004
#[test]
fn republish_under_a_held_lock_writes_nothing() {
    // When another Sheets-mutating primitive holds the advisory lock, republish must
    // NOT write concurrently: nothing lands, and the outcome reports not-published —
    // a non-destructive refusal retried on the next sync. (SHEET-PUB-004)
    let mut pub_ = Publisher::new(InMemorySheetsView::new());

    let t = tab(POSITIONS_TAB, rows(&[("AMZN", "3")]));
    let outcome = pub_.republish(&[t], &HeldLock, "2022-09-01");

    assert!(!outcome.published, "a held lock refuses the republish");
    assert!(
        pub_.client().published(POSITIONS_TAB).is_none(),
        "a republish under a held lock writes nothing"
    );
}
