//! The replay -> project -> read-marks -> cache cycle: RUNTIME-CYCLE-001/002/003.
//! Driven with the FAKE `sheets-view` client (no network) and REAL ledger-core /
//! tax / config types.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::collections::BTreeMap;

use common::{closed_symbol_logs, ctx, small_logs};

use pt_core::Date;
use reports::TradingDayKey;
use runtime::cycle::{
    per_symbol_freshness, reduce_trading_day_key, replay_with_cached_marks, run_cycle, MarksCache,
    SymbolFreshness,
};
use sheets_view::testkit::{pass, InMemorySheetsView};
use sheets_view::{DegradeReason, Mark, PriceReading, SettleConfig};

use pt_core::Cents;

const AS_OF: Date = Date(19_200);

// @spec RUNTIME-CYCLE-001, RUNTIME-BOOT-002
#[test]
fn runtime_loads_the_event_log_via_store_then_replays_and_reads_marks_back() {
    // runtime genuinely owns the LOAD step: load_and_run_cycle loads the event log
    // via store (the workbook wins), then replays + reads marks back through
    // run_cycle — not receiving a pre-built EventLogs. Driven through a FAKE store
    // (in-memory sheets + cache + no-op lock). (RUNTIME-CYCLE-001)
    use runtime::cycle::{load_and_run_cycle, CycleError};
    use store::testkit::InMemorySheets;
    use store::{serde_rows, InMemoryCache, NoopLock, Store};

    // Seed the authoritative workbook with the small two-symbol ledger.
    let logs = small_logs();
    let ledger_rows: Vec<_> = logs.ledger.iter().map(serde_rows::ledger_to_row).collect();
    let sheets = InMemorySheets::seeded(ledger_rows, vec![]);
    let mut store = Store::new(sheets, NoopLock::new(), InMemoryCache::new());

    let ctx = ctx(2022);
    let settle = SettleConfig { max_polls: 4 };
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[
        ("AMZN", PriceReading::Numeric { price_usd: 170.0, quote_date: Date(19_180) }),
        ("GOOG", PriceReading::Numeric { price_usd: 125.0, quote_date: Date(19_181) }),
    ]));

    // runtime loads via store, then runs the full cycle.
    let out = load_and_run_cycle(&mut store, &MarksCache::new(), &ctx, AS_OF, &client, settle)
        .expect("load + cycle");

    // The Snapshot replayed from the LOADED log: AMZN open (3 shares), the GOOG sale
    // realized a gain (so accruals exist), and this run's marks were read back.
    assert!(out.snapshot.positions.contains_key("AMZN"), "the loaded log replayed");
    assert!(!out.accruals.is_empty(), "tax replayed over the loaded realized gains");
    assert_eq!(out.marks.marks.get("AMZN").unwrap().price_cents, Cents(170_00));

    // A store load failure surfaces as CycleError::Load (the workbook unreachable).
    store.sheets_mut().set_unreachable(true);
    match load_and_run_cycle(&mut store, &MarksCache::new(), &ctx, AS_OF, &client, settle) {
        Err(CycleError::Load(_)) => {}
        other => panic!("an offline workbook must surface as CycleError::Load, got {other:?}"),
    }
}

// @spec RUNTIME-CYCLE-001
#[test]
fn runtime_is_the_replay_caller_holding_snapshot_accruals_and_estimates() {
    // runtime loads the log, replays ledger-core then tax with config and the
    // prior cached marks, and HOLDS the Snapshot + accruals + annual rows + the
    // per-symbol unrealized estimates the consumers render. (RUNTIME-CYCLE-001)
    let logs = small_logs();
    let ctx = ctx(2022);

    // The prior cycle's cached marks (AMZN priced; GOOG priced).
    let mut prior = MarksCache::new();
    prior.marks.insert("AMZN".to_string(), Mark { price_cents: Cents(160_00), quote_date: Date(19_150) });
    prior.marks.insert("GOOG".to_string(), Mark { price_cents: Cents(120_00), quote_date: Date(19_150) });

    let replayed = replay_with_cached_marks(&logs, &prior, &ctx, AS_OF);

    // The Snapshot replayed: AMZN (3 shares open), GOOG (1 share open after the sale).
    let amzn = replayed.snapshot.positions.get("AMZN").expect("AMZN position");
    assert_eq!(amzn.total_qty, pt_core::MicroShares(3_000_000));
    // The injected mark valued AMZN's unrealized (160 - 150 = 10/share over 3 shares).
    assert_eq!(amzn.unrealized_cents, Some(Cents(30_00)));

    // The realized GOOG sale produced a realized gain → tax accruals exist.
    assert!(!replayed.snapshot.realized_gains.is_empty(), "the GOOG sale realizes a gain");
    assert!(!replayed.accruals.is_empty(), "tax holds accruals over the realized gains");
    assert!(!replayed.annual_rows.is_empty(), "tax holds the annual reserve rows");

    // The per-symbol unrealized estimate is held for the open positions.
    assert!(replayed.estimates.contains_key("AMZN"), "an unrealized estimate is held per open symbol");
}

// @spec RUNTIME-CYCLE-001
#[test]
fn degraded_prior_mark_is_absent_from_injected_replay_never_zero() {
    // A symbol absent from the cached marks (degraded) is injected as ABSENT, so
    // ledger-core degrades it per-symbol (unrealized None), never valued at zero.
    // (RUNTIME-CYCLE-001/002)
    let logs = small_logs();
    let ctx = ctx(2022);

    let mut prior = MarksCache::new();
    // Only GOOG priced; AMZN degraded (absent + recorded degraded).
    prior.marks.insert("GOOG".to_string(), Mark { price_cents: Cents(120_00), quote_date: Date(19_150) });
    prior.degraded.insert("AMZN".to_string(), DegradeReason::TimedOut);

    let replayed = replay_with_cached_marks(&logs, &prior, &ctx, AS_OF);
    let amzn = replayed.snapshot.positions.get("AMZN").expect("AMZN position");
    assert_eq!(amzn.unrealized_cents, None, "a degraded symbol is unvalued (None), never zero");
}

// @spec RUNTIME-CYCLE-002
#[test]
fn marks_read_back_are_cached_and_injected_into_the_next_replay() {
    // The cycle reads marks back via sheets-view and caches them; the cache is what
    // the NEXT cycle injects — closing the produce-after-consume loop. The injection
    // is observable through the transient carry-forward (SHEET-MARK-003): a symbol
    // transient on cycle 2 keeps cycle 1's cached mark rather than being nulled,
    // and the held outcome values it at that carried mark. (RUNTIME-CYCLE-002)
    let logs = small_logs();
    let ctx = ctx(2022);
    let settle = SettleConfig { max_polls: 4 };

    // Cycle 1: no prior marks; sheets-view reads back AMZN=170, GOOG=125.
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[
        ("AMZN", PriceReading::Numeric { price_usd: 170.0, quote_date: Date(19_180) }),
        ("GOOG", PriceReading::Numeric { price_usd: 125.0, quote_date: Date(19_181) }),
    ]));
    let prior = MarksCache::new();
    let out1 = run_cycle(&logs, &prior, &ctx, AS_OF, &client, settle).expect("cycle 1");

    // The read-back marks are cached on the outcome.
    assert_eq!(out1.marks.marks.get("AMZN").unwrap().price_cents, Cents(170_00));
    assert_eq!(out1.marks.marks.get("GOOG").unwrap().price_cents, Cents(125_00));

    // Cycle 2: inject cycle-1's cached marks. AMZN stays TRANSIENT past the window;
    // the injected prior cache carries its 170 mark forward (never a null), while
    // GOOG settles fresh at 130.
    let client2 = InMemorySheetsView::new();
    client2.set_prices(pass(&[
        ("AMZN", PriceReading::Transient),
        ("GOOG", PriceReading::Numeric { price_usd: 130.0, quote_date: Date(19_190) }),
    ]));
    let out2 = run_cycle(&logs, &out1.marks, &ctx, AS_OF, &client2, settle).expect("cycle 2");

    // AMZN kept cycle-1's injected mark (170, with its quote stamp) — the loop closed.
    assert_eq!(out2.marks.marks.get("AMZN").unwrap().price_cents, Cents(170_00));
    assert_eq!(out2.marks.marks.get("AMZN").unwrap().quote_date, Date(19_180));
    // The held snapshot is valued at THIS run's cache (RUNTIME-CYCLE-006): AMZN at
    // the carried 170 → (170 − 150) × 3 = 60.00 unrealized.
    let amzn = out2.snapshot.positions.get("AMZN").unwrap();
    assert_eq!(
        amzn.unrealized_cents,
        Some(Cents(60_00)),
        "the transiently-unknown symbol is valued at the carried prior mark"
    );
    // And cycle 2 read back GOOG's NEW mark (130) for cycle 3.
    assert_eq!(out2.marks.marks.get("GOOG").unwrap().price_cents, Cents(130_00));
}

// @spec RUNTIME-CYCLE-006
#[test]
fn one_shot_run_outcome_is_valued_at_this_runs_read_back_marks() {
    // A one-shot process run (the cron summary, a fresh TUI launch) has NO
    // persisted prior cache: the pre-read replay's injected marks are empty. The
    // cycle closes the loop WITHIN the run — re-replaying with this run's read-back
    // marks — so the held Snapshot and tax estimates render PRICED, not
    // blanket-degraded. (RUNTIME-CYCLE-006)
    let logs = small_logs();
    let ctx = ctx(2022);
    let settle = SettleConfig { max_polls: 4 };
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[
        ("AMZN", PriceReading::Numeric { price_usd: 170.0, quote_date: Date(19_180) }),
        ("GOOG", PriceReading::Numeric { price_usd: 125.0, quote_date: Date(19_181) }),
    ]));

    let out = run_cycle(&logs, &MarksCache::new(), &ctx, AS_OF, &client, settle).expect("cycle");

    // The snapshot is valued at the marks read back THIS run: AMZN 3 shares at
    // (170 − 150) = 60.00 unrealized — never None/degraded.
    let amzn = out.snapshot.positions.get("AMZN").unwrap();
    assert_eq!(amzn.unrealized_cents, Some(Cents(60_00)));

    // The tax estimate is computable (a priced mark), so composition never
    // degrades the symbol as NoTaxEstimate (which would null every value column).
    let est = out.estimates.get("AMZN").expect("estimate held");
    assert!(
        est.estimated_tax_cents.is_some(),
        "a priced symbol carries a real estimate, got {est:?}"
    );

    // And the trading-day key reduces from this run's quote epochs.
    assert_eq!(out.trading_day_key, Some(TradingDayKey(Date(19_181))));
}

// @spec RUNTIME-CYCLE-002
#[test]
fn offline_read_back_propagates_so_caller_retains_prior_marks() {
    // On an offline marks read-back, run_cycle propagates the Err; runtime does NOT
    // silently emit an empty marks set — the caller retains its prior cached marks
    // (and re-injects them next cycle), per the boundary contract. (RUNTIME-CYCLE-002)
    let logs = small_logs();
    let ctx = ctx(2022);
    let settle = SettleConfig { max_polls: 4 };

    let mut prior = MarksCache::new();
    prior.marks.insert("AMZN".to_string(), Mark { price_cents: Cents(160_00), quote_date: Date(19_150) });

    let client = InMemorySheetsView::new();
    client.set_read_fails(true); // the workbook is unreachable / offline.

    let result = run_cycle(&logs, &prior, &ctx, AS_OF, &client, settle);
    assert!(result.is_err(), "offline read-back propagates; no empty marks set is emitted");

    // The caller still holds its prior marks (run_cycle did not consume/clear them),
    // so it can re-inject them next cycle.
    assert_eq!(prior.marks.get("AMZN").unwrap().price_cents, Cents(160_00));
}

// @spec RUNTIME-CYCLE-002, RUNTIME-CYCLE-004
#[test]
fn a_fully_closed_symbol_is_not_priced_nor_recorded_degraded() {
    // ledger-core::replay keeps a fully-disposed symbol (qty 0) in positions (via
    // realized.keys()), but a closed position is not on the Positions tab — so the
    // cycle must NOT request a mark for it. If it did, the symbol (with no price
    // reading and no prior mark) would be recorded Degraded(TimedOut) and would
    // pollute per-symbol freshness + the trading-day reduction. The marks request is
    // filtered to OPEN positions (qty != 0). (RUNTIME-CYCLE-002)
    let logs = closed_symbol_logs(); // GOOG fully sold (qty 0); AMZN open.
    let ctx = ctx(2022);
    let settle = SettleConfig { max_polls: 4 };

    // Confirm GOOG is in the snapshot at qty 0 (the precondition the bug rode on).
    let replayed = replay_with_cached_marks(&logs, &MarksCache::new(), &ctx, AS_OF);
    let goog = replayed.snapshot.positions.get("GOOG").expect("GOOG kept in positions");
    assert_eq!(goog.total_qty, pt_core::MicroShares(0), "GOOG is fully disposed");

    // The settle pass prices only AMZN. GOOG is absent from the script entirely; if
    // the cycle erroneously requested GOOG, read_marks would degrade it (TimedOut).
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[(
        "AMZN",
        PriceReading::Numeric { price_usd: 170.0, quote_date: Date(19_180) },
    )]));

    let out = run_cycle(&logs, &MarksCache::new(), &ctx, AS_OF, &client, settle).expect("cycle");

    // The closed GOOG is neither priced nor recorded degraded.
    assert!(out.marks.marks.contains_key("AMZN"), "the open symbol is priced");
    assert!(!out.marks.marks.contains_key("GOOG"), "a closed symbol is not priced");
    assert!(
        !out.marks.degraded.contains_key("GOOG"),
        "a closed symbol is not recorded degraded — it was never requested"
    );

    // It does not appear in per-symbol freshness.
    let fresh = per_symbol_freshness(&out.marks);
    assert!(!fresh.contains_key("GOOG"), "a closed symbol is absent from per-symbol freshness");

    // The trading-day key is AMZN's quote-epoch only (a closed symbol contributes
    // nothing to the reduction).
    assert_eq!(out.trading_day_key, Some(TradingDayKey(Date(19_180))));
}

// @spec RUNTIME-CYCLE-003
#[test]
fn quote_epochs_reduce_to_one_trading_day_key_most_recent_across_priced() {
    // runtime reduces the per-symbol GOOGLEFINANCE quote dates to ONE trading-day
    // key: the most-recent quote-epoch across priced symbols. (RUNTIME-CYCLE-003)
    let mut cache = MarksCache::new();
    cache.marks.insert("AMZN".to_string(), Mark { price_cents: Cents(170_00), quote_date: Date(19_180) });
    cache.marks.insert("GOOG".to_string(), Mark { price_cents: Cents(125_00), quote_date: Date(19_181) });
    cache.marks.insert("MSFT".to_string(), Mark { price_cents: Cents(300_00), quote_date: Date(19_179) });

    // The single key is the MOST-RECENT quote-epoch (19_181), not the earliest.
    assert_eq!(reduce_trading_day_key(&cache), Some(TradingDayKey(Date(19_181))));
}

// @spec RUNTIME-CYCLE-003
#[test]
fn no_priced_symbol_keys_nothing_rather_than_fabricating_a_day() {
    // When every symbol is degraded (no priced mark), there is no trading day to
    // key — runtime returns None rather than fabricating the run's calendar day.
    // (RUNTIME-CYCLE-003)
    let mut cache = MarksCache::new();
    cache.degraded.insert("AMZN".to_string(), DegradeReason::Permanent);
    cache.degraded.insert("GOOG".to_string(), DegradeReason::TimedOut);
    assert_eq!(reduce_trading_day_key(&cache), None);
}

// @spec RUNTIME-CYCLE-003
#[test]
fn per_symbol_stamp_and_degraded_flag_are_preserved_through_the_reduction() {
    // The single trading-day key is the reduction, but each symbol's OWN quote
    // stamp and degraded flag survive for per-symbol freshness display.
    // (RUNTIME-CYCLE-003)
    let mut cache = MarksCache::new();
    cache.marks.insert("AMZN".to_string(), Mark { price_cents: Cents(170_00), quote_date: Date(19_180) });
    cache.marks.insert("GOOG".to_string(), Mark { price_cents: Cents(125_00), quote_date: Date(19_181) });
    cache.degraded.insert("MSFT".to_string(), DegradeReason::Permanent);

    let reduced = reduce_trading_day_key(&cache).unwrap();
    assert_eq!(reduced, TradingDayKey(Date(19_181)));

    let fresh: BTreeMap<_, _> = per_symbol_freshness(&cache);
    // AMZN keeps its OWN (earlier) stamp even though the reduced key is GOOG's.
    assert_eq!(
        fresh.get("AMZN"),
        Some(&SymbolFreshness::Priced { quote_epoch: Date(19_180) }),
        "AMZN's own stamp survives the reduction"
    );
    assert_eq!(
        fresh.get("GOOG"),
        Some(&SymbolFreshness::Priced { quote_epoch: Date(19_181) })
    );
    // MSFT's degraded flag is preserved.
    assert_eq!(
        fresh.get("MSFT"),
        Some(&SymbolFreshness::Degraded { reason: DegradeReason::Permanent }),
        "MSFT's degraded flag survives the reduction"
    );
}

// @spec RUNTIME-CYCLE-003
#[test]
fn priced_marks_projection_feeds_history_keyed_by_the_reduced_trading_day_key() {
    // The consumer side of the reduction: the marks cache projects to
    // reports::PricedMarks (price + quote_epoch) via to_priced_marks, and the cycle's
    // reduced trading_day_key keys the History capture — while each PricedMark keeps
    // its OWN per-symbol quote_epoch. This proves the projection is live and that
    // reports keys the snapshot by the runtime-reduced trading-day key, carrying the
    // per-symbol stamps. (RUNTIME-CYCLE-003)
    use reports::{append_snapshot, build_series_point, read_history, TradingDayKey};
    use reports::testkit::{InMemoryHistory, NoopLock};

    let logs = small_logs();
    let ctx = ctx(2022);
    let settle = SettleConfig { max_polls: 4 };

    // A cycle reads back AMZN (epoch 19_180) and GOOG (epoch 19_183); the reduced
    // trading-day key is the most-recent (19_183).
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[
        ("AMZN", PriceReading::Numeric { price_usd: 170.0, quote_date: Date(19_180) }),
        ("GOOG", PriceReading::Numeric { price_usd: 125.0, quote_date: Date(19_183) }),
    ]));
    let out = run_cycle(&logs, &MarksCache::new(), &ctx, AS_OF, &client, settle).expect("cycle");
    let key = out.trading_day_key.expect("a priced cycle has a key");
    assert_eq!(key, TradingDayKey(Date(19_183)));

    // Project the cached marks to reports::PricedMarks (the live to_priced_marks
    // path) and build the series point keyed by the runtime-reduced key.
    let priced = out.marks.to_priced_marks();
    assert_eq!(
        priced.get("AMZN").unwrap().quote_epoch,
        Date(19_180),
        "AMZN's own per-symbol quote-epoch survives into the priced-marks projection"
    );
    assert_eq!(priced.get("GOOG").unwrap().quote_epoch, Date(19_183));

    let point = build_series_point(
        &out.snapshot,
        &priced,
        &out.estimates,
        key,
        1_700_000_000,
        Date(19_183),
    );

    // Capture it through the in-memory History client and read it back: reports keys
    // the durable series by the runtime-reduced trading-day key, carrying the marks
    // (with their per-symbol quote-epochs).
    let mut history = InMemoryHistory::new();
    let lock = NoopLock::new();
    append_snapshot(&mut history, &lock, &point).expect("History capture");
    let series = read_history(&history).expect("read the durable series");
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].key, key, "History keys the point by the runtime-reduced trading-day key");
    assert_eq!(
        series[0].marks.get("AMZN").unwrap().quote_epoch,
        Date(19_180),
        "the per-symbol quote-epoch is carried into the durable point"
    );
}

// @spec RUNTIME-CYCLE-003
#[test]
fn run_cycle_reduces_the_read_back_quote_epochs_to_the_trading_day_key() {
    // End-to-end: the cycle reads marks back (with their per-symbol quote dates) and
    // the outcome's trading_day_key is the most-recent across priced symbols.
    // (RUNTIME-CYCLE-003)
    let logs = small_logs();
    let ctx = ctx(2022);
    let settle = SettleConfig { max_polls: 4 };

    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[
        ("AMZN", PriceReading::Numeric { price_usd: 170.0, quote_date: Date(19_180) }),
        ("GOOG", PriceReading::Numeric { price_usd: 125.0, quote_date: Date(19_183) }),
    ]));
    let out = run_cycle(&logs, &MarksCache::new(), &ctx, AS_OF, &client, settle).expect("cycle");

    assert_eq!(
        out.trading_day_key,
        Some(TradingDayKey(Date(19_183))),
        "the cycle keys the run by the most-recent priced quote-epoch"
    );
}
