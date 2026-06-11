//! The History-append retry-and-flag loop (RUNTIME-REPORTS-001) and the
//! post-priced-cycle History capture trigger (RUNTIME-CYCLE-005). reports only
//! FLAGS a single failure; runtime owns the bounded retry-with-backoff loop under
//! the advisory write-lock, and on continued failure surfaces the trading-day point
//! as uncaptured/flagged rather than dropping it. After a priced cycle runtime
//! triggers the capture (trading-day key + to_priced_marks), last-wins per key.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;

use common::{ctx, small_logs};

use pt_core::{Cents, Date, MicroShares};
use reports::testkit::{InMemoryHistory, NoopLock};
use reports::{
    read_history, HistoryClient, HistoryError, HistoryRow, PricedMark, SeriesPoint, TradingDayKey,
};
use runtime::{
    capture_cycle, capture_with_retry, load_run_and_capture, run_cycle, BackoffPolicy,
    CaptureOutcome, MarksCache,
};
use sheets_view::testkit::{pass, InMemorySheetsView};
use sheets_view::{PriceReading, SettleConfig};

const AS_OF: Date = Date(19_200);

fn series_point(key_days: i32, mv_cents: i64) -> SeriesPoint {
    let mut marks: BTreeMap<String, PricedMark> = BTreeMap::new();
    marks.insert(
        "AMZN".to_string(),
        PricedMark {
            price_cents: Cents(170_00),
            quote_epoch: Date(key_days),
        },
    );
    let mut per_symbol_value = BTreeMap::new();
    per_symbol_value.insert("AMZN".to_string(), Cents(mv_cents));
    let mut per_symbol_shares = BTreeMap::new();
    per_symbol_shares.insert("AMZN".to_string(), MicroShares(3_000_000));
    SeriesPoint {
        key: TradingDayKey(Date(key_days)),
        total_market_value_cents: Cents(mv_cents),
        total_unrealized_pretax_cents: Cents(30_00),
        total_unrealized_net_of_tax_cents: Cents(20_00),
        total_basis_cents: Cents(450_00),
        per_symbol_value_cents: per_symbol_value,
        per_symbol_shares,
        marks,
        captured_at_epoch_secs: 1_700_000_000,
        reporting_tz_date: Date(key_days),
        incomplete: false,
    }
}

/// A History client that fails its first `fail_first` upserts (modeling a transient
/// write failure), then succeeds — so the retry loop's recovery can be observed.
/// (RUNTIME-REPORTS-001)
struct FlakyHistory {
    inner: InMemoryHistory,
    fail_first: RefCell<u32>,
    upsert_calls: RefCell<u32>,
}

impl FlakyHistory {
    fn new(fail_first: u32) -> Self {
        FlakyHistory {
            inner: InMemoryHistory::new(),
            fail_first: RefCell::new(fail_first),
            upsert_calls: RefCell::new(0),
        }
    }
}

impl HistoryClient for FlakyHistory {
    fn read_history(&self) -> Result<Vec<HistoryRow>, HistoryError> {
        self.inner.read_history()
    }
    fn upsert_point(&mut self, row: &HistoryRow) -> Result<(), HistoryError> {
        *self.upsert_calls.borrow_mut() += 1;
        let mut remaining = self.fail_first.borrow_mut();
        if *remaining > 0 {
            *remaining -= 1;
            return Err(HistoryError::WriteVerifyFailed);
        }
        self.inner.upsert_point(row)
    }
}

// @spec RUNTIME-REPORTS-001
#[test]
fn a_transient_write_failure_is_retried_under_the_loop_then_succeeds() {
    // reports only flags a single WriteVerifyFailed; runtime's loop retries with
    // backoff and the point lands on a later attempt. The backoff sleeps are recorded
    // (no real waiting). (RUNTIME-REPORTS-001)
    let mut history = FlakyHistory::new(2); // fail twice, then succeed.
    let lock = NoopLock::new();
    let policy = BackoffPolicy {
        max_attempts: 5,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(8),
    };
    let slept: RefCell<Vec<Duration>> = RefCell::new(Vec::new());

    let point = series_point(19_180, 510_00);
    let outcome = capture_with_retry(&mut history, &lock, &point, &policy, |d| {
        slept.borrow_mut().push(d)
    });

    assert_eq!(
        outcome,
        CaptureOutcome::Captured,
        "the point lands on a retry"
    );
    assert!(outcome.is_captured());
    // Exactly three upsert attempts (fail, fail, succeed) and two backoff sleeps.
    assert_eq!(
        *history.upsert_calls.borrow(),
        3,
        "the loop retried under failure"
    );
    assert_eq!(slept.borrow().len(), 2, "two retries → two backoff sleeps");

    // The point is durably present.
    let series = read_history(&history).expect("read back");
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].key, point.key);
}

// @spec RUNTIME-REPORTS-001
#[test]
fn continued_failure_surfaces_the_point_as_uncaptured_not_dropped() {
    // A write that never durably confirms exhausts the bounded budget; runtime
    // surfaces the trading-day point as uncaptured/flagged (carrying its key) rather
    // than discarding a non-reconstructable point. (RUNTIME-REPORTS-001)
    let mut history = InMemoryHistory::new();
    history.set_write_fails(true); // the write never confirms.
    let lock = NoopLock::new();
    let policy = BackoffPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };

    let point = series_point(19_181, 600_00);
    let outcome = capture_with_retry(&mut history, &lock, &point, &policy, |_| {});

    match outcome {
        CaptureOutcome::Uncaptured { key, attempts } => {
            assert_eq!(
                key, point.key,
                "the uncaptured flag carries the trading-day key"
            );
            assert_eq!(attempts, 3, "the loop tried exactly the bounded budget");
        }
        other => panic!("continued failure must surface uncaptured, got {other:?}"),
    }
    // Nothing was durably captured — but it was FLAGGED, not silently dropped.
    assert_eq!(
        history.row_count(),
        0,
        "no point landed (it is flagged uncaptured)"
    );
}

// @spec RUNTIME-REPORTS-001
#[test]
fn a_held_write_lock_is_retried_then_flagged_uncaptured() {
    // append_snapshot acquires the write-lock inside itself (RUNTIME-LOCK-002); a
    // held lock surfaces as WriteVerifyFailed, which the loop retries up to the budget
    // and then flags uncaptured (a cron capture cannot steal an open TUI's lock).
    // (RUNTIME-REPORTS-001, RUNTIME-LOCK-002)
    use pt_core::{LockError, LockGuard};
    struct HeldLock;
    impl store::Lock for HeldLock {
        fn acquire(&self) -> Result<LockGuard, LockError> {
            Err(LockError::Held)
        }
    }
    let mut history = InMemoryHistory::new();
    let policy = BackoffPolicy {
        max_attempts: 2,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };
    let outcome = capture_with_retry(
        &mut history,
        &HeldLock,
        &series_point(19_182, 1),
        &policy,
        |_| {},
    );
    assert!(
        outcome.is_uncaptured(),
        "a persistently-held lock flags the point uncaptured"
    );
    assert_eq!(
        history.row_count(),
        0,
        "nothing appended while the lock is held"
    );
}

// @spec RUNTIME-CYCLE-005, RUNTIME-REPORTS-001
#[test]
fn a_priced_cycle_triggers_the_history_capture_under_the_lock() {
    // After a cycle that produced priced marks, runtime triggers the capture: it keys
    // the durable point by the reduced trading-day key and the to_priced_marks
    // projection, appending under the advisory write-lock. (RUNTIME-CYCLE-005)
    let logs = small_logs();
    let ctx = ctx(2022);
    let settle = SettleConfig { max_polls: 4 };

    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[
        (
            "AMZN",
            PriceReading::Numeric {
                price_usd: 170.0,
                quote_date: Date(19_180),
            },
        ),
        (
            "GOOG",
            PriceReading::Numeric {
                price_usd: 125.0,
                quote_date: Date(19_183),
            },
        ),
    ]));
    let out = run_cycle(&logs, &MarksCache::new(), &ctx, AS_OF, &client, settle).expect("cycle");
    let key = out.trading_day_key.expect("a priced cycle has a key");

    let mut history = InMemoryHistory::new();
    let lock = NoopLock::new();
    let policy = BackoffPolicy::default();
    let captured = capture_cycle(
        &mut history,
        &lock,
        &out,
        &out.estimates,
        Date(19_183),
        1_700_000_000,
        &policy,
        |_| {},
    );

    assert_eq!(
        captured,
        Some(CaptureOutcome::Captured),
        "a priced cycle is captured"
    );
    let series = read_history(&history).expect("read back");
    assert_eq!(
        series.len(),
        1,
        "the priced cycle produced one durable point"
    );
    assert_eq!(
        series[0].key, key,
        "History keys the point by the runtime-reduced trading-day key"
    );
}

// @spec RUNTIME-CYCLE-005
#[test]
fn a_degraded_only_cycle_triggers_no_capture() {
    // A cycle with no priced marks keyed nothing (RUNTIME-CYCLE-003); runtime
    // triggers NO capture rather than fabricating a calendar-day point.
    // (RUNTIME-CYCLE-005)
    let logs = small_logs();
    let ctx = ctx(2022);
    let settle = SettleConfig { max_polls: 1 };

    // Every symbol degrades (no numeric reading, no prior mark) → no trading-day key.
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[
        ("AMZN", PriceReading::Permanent),
        ("GOOG", PriceReading::Permanent),
    ]));
    let out = run_cycle(&logs, &MarksCache::new(), &ctx, AS_OF, &client, settle).expect("cycle");
    assert_eq!(
        out.trading_day_key, None,
        "a degraded-only cycle keys nothing"
    );

    let mut history = InMemoryHistory::new();
    let lock = NoopLock::new();
    let captured = capture_cycle(
        &mut history,
        &lock,
        &out,
        &out.estimates,
        AS_OF,
        1_700_000_000,
        &BackoffPolicy::default(),
        |_| {},
    );
    assert_eq!(captured, None, "no priced marks → no capture triggered");
    assert_eq!(
        history.row_count(),
        0,
        "nothing captured for a degraded-only cycle"
    );
}

// @spec RUNTIME-CYCLE-005, REPORT-HIST-002
#[test]
fn a_re_triggered_capture_for_the_same_trading_day_reconciles_last_wins() {
    // A TUI-triggered and a cron-triggered capture for the SAME trading day reconcile
    // to ONE point (History last-wins per trading-day key), never a duplicate.
    // (RUNTIME-CYCLE-005, REPORT-HIST-002)
    let mut history = InMemoryHistory::new();
    let lock = NoopLock::new();
    let policy = BackoffPolicy::default();

    // Two captures for the SAME day with different values.
    let first = capture_with_retry(
        &mut history,
        &lock,
        &series_point(19_180, 500_00),
        &policy,
        |_| {},
    );
    let second = capture_with_retry(
        &mut history,
        &lock,
        &series_point(19_180, 999_00),
        &policy,
        |_| {},
    );
    assert!(first.is_captured() && second.is_captured());

    let series = read_history(&history).expect("read back");
    assert_eq!(
        series.len(),
        1,
        "last-wins: the same trading day did not duplicate"
    );
    assert_eq!(
        series[0].total_market_value_cents,
        Cents(999_00),
        "the later capture overwrote"
    );
}

// @spec RUNTIME-CYCLE-005, RUNTIME-REPORTS-001
#[test]
fn load_run_and_capture_drives_the_cycle_then_the_capture() {
    // The full produce-then-persist path: load + run the cycle, then trigger the
    // capture under the lock. Driven through a FAKE store + view client + in-memory
    // History (no network). (RUNTIME-CYCLE-005)
    use store::testkit::InMemorySheets;
    use store::{serde_rows, InMemoryCache, NoopLock as StoreNoopLock, Store};

    let logs = small_logs();
    let ledger_rows: Vec<_> = logs.ledger.iter().map(serde_rows::ledger_to_row).collect();
    let sheets = InMemorySheets::seeded(ledger_rows, vec![]);
    let mut store = Store::new(sheets, StoreNoopLock::new(), InMemoryCache::new());

    let ctx = ctx(2022);
    let view = InMemorySheetsView::new();
    view.set_prices(pass(&[
        (
            "AMZN",
            PriceReading::Numeric {
                price_usd: 170.0,
                quote_date: Date(19_180),
            },
        ),
        (
            "GOOG",
            PriceReading::Numeric {
                price_usd: 125.0,
                quote_date: Date(19_181),
            },
        ),
    ]));

    let mut history = InMemoryHistory::new();
    let hist_lock = NoopLock::new();
    let captured = load_run_and_capture(
        &mut store,
        &MarksCache::new(),
        &ctx,
        AS_OF,
        &view,
        SettleConfig { max_polls: 4 },
        &mut history,
        &hist_lock,
        &BackoffPolicy::default(),
        Date(19_181),
        1_700_000_000,
        |_| {},
    )
    .expect("load + run + capture");

    assert!(
        captured.outcome.snapshot.positions.contains_key("AMZN"),
        "the cycle replayed"
    );
    assert_eq!(
        captured.capture,
        Some(CaptureOutcome::Captured),
        "the priced cycle was captured"
    );
    assert_eq!(
        read_history(&history).expect("read back").len(),
        1,
        "one durable point landed"
    );
}
