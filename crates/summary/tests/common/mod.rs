//! Shared test fixtures for the `summary` suite: a small replayed portfolio, priced
//! marks with quote-epochs, `tax` unrealized estimates + annual rows, stored History
//! points, and `SummaryInputs` builders. No `#[test]`s here. Mirrors the `reports`
//! suite's fixture idioms.
#![allow(dead_code)]
#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeMap;

use config::{BracketRow, BracketSet, BracketState, Jurisdiction, Niit, Ppm, StateCode, TaxYear};
use ledger_core::{LedgerEvent, LedgerEventKind, Marks, Snapshot, Symbol};
use pt_core::{Cents, Date, MicroShares, Seq};
use reports::{
    build_series_point, point_checksum, HistoryRow, PricedMark, PricedMarks, SeriesPoint,
    TradingDayKey,
};
use summary::SummaryInputs;
use tax::{
    AccrualKey, AnnualRow, Quarter, ResolvedJurisdiction, TaxContext, TaxEvent, TaxEventKind,
    UnrealizedEstimate,
};

// ---------------------------------------------------------------------------
// Ledger events.
// ---------------------------------------------------------------------------

/// A `Buy` event opening a lot on `platform`.
pub fn buy(
    seq: u64,
    date: i32,
    lot: &str,
    symbol: &str,
    qty_micro: i64,
    unit_cents: i64,
    platform: &str,
) -> LedgerEvent {
    LedgerEvent {
        id: format!("e{seq}"),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Buy {
            lot_id: lot.to_string(),
            symbol: symbol.to_string(),
            qty: MicroShares(qty_micro),
            unit_price_cents: Cents(unit_cents),
            fees_cents: Cents(0),
            platform: platform.to_string(),
            tracking_code: None,
        },
    }
}

/// A `Sell` event (FIFO) disposing `qty_micro` shares of `symbol` on `platform`.
pub fn sell(
    seq: u64,
    date: i32,
    sale: &str,
    symbol: &str,
    qty_micro: i64,
    unit_cents: i64,
    platform: &str,
    state: Option<&str>,
) -> LedgerEvent {
    LedgerEvent {
        id: format!("e{seq}"),
        seq: Seq(seq),
        date: Date(date),
        kind: LedgerEventKind::Sell {
            sale_id: sale.to_string(),
            symbol: symbol.to_string(),
            qty: MicroShares(qty_micro),
            unit_price_cents: Cents(unit_cents),
            fees_cents: Cents(0),
            lot_refs: vec![],
            accrues_to_state: state.map(|s| s.to_string()),
            platform: platform.to_string(),
            tracking_code: None,
        },
    }
}

/// A `Marks` map for replay (symbol → per-share Cents).
pub fn ledger_marks(entries: &[(&str, i64)]) -> Marks {
    entries
        .iter()
        .map(|(s, c)| (Symbol::from(*s), Cents(*c)))
        .collect()
}

/// Priced marks (with quote-epoch) for composition/capture: `(symbol, cents,
/// quote_epoch_day)`. A symbol omitted is degraded (no mark).
pub fn priced_marks(entries: &[(&str, i64, i32)]) -> PricedMarks {
    entries
        .iter()
        .map(|(s, c, q)| {
            (
                Symbol::from(*s),
                PricedMark {
                    price_cents: Cents(*c),
                    quote_epoch: Date(*q),
                },
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// A small two-symbol portfolio across two platforms.
//   AMZN: 3 shares bought @ $150 on schwab (open).
//   GOOG: 2 shares bought @ $100 on fidelity, 1 sold @ $130 (1 open).
// ---------------------------------------------------------------------------

/// The events of the small portfolio.
pub fn small_events() -> Vec<LedgerEvent> {
    vec![
        buy(1, 19_000, "lot-amzn", "AMZN", 3_000_000, 150_00, "schwab"),
        buy(2, 19_010, "lot-goog", "GOOG", 2_000_000, 100_00, "fidelity"),
        sell(
            3,
            19_100,
            "sale-1",
            "GOOG",
            1_000_000,
            130_00,
            "fidelity",
            Some("NJ"),
        ),
    ]
}

/// The small portfolio replayed with `marks` (symbol → per-share Cents).
pub fn small_snapshot(marks: &Marks) -> Snapshot {
    ledger_core::replay(&small_events(), marks)
}

// ---------------------------------------------------------------------------
// Tax context + unrealized estimates + annual rows.
// ---------------------------------------------------------------------------

fn flat_set(rate: i64) -> BracketSet {
    BracketSet {
        rows: vec![BracketRow {
            lower_threshold_cents: Cents(0),
            rate_ppm: Ppm(rate),
        }],
        last_verified: Date(19_000),
        source_note: "test".to_string(),
    }
}

/// A federal+NJ tax context for `tax_year`, with flat ordinary/LT/state rates.
pub fn ctx(tax_year: i32) -> TaxContext {
    let federal = ResolvedJurisdiction {
        jurisdiction: Jurisdiction::Federal,
        ordinary: Some(flat_set(370_000)),          // 37%
        federal_long_term: Some(flat_set(200_000)), // 20%
        niit: Some(Niit {
            rate_ppm: Ppm(38_000),
            magi_threshold_cents: Cents(0),
        }),
        ordinary_income_cents: Cents(0),
        state: BracketState::Verified,
    };
    let nj = ResolvedJurisdiction {
        jurisdiction: Jurisdiction::State("NJ".to_string()),
        ordinary: Some(flat_set(55_250)), // 5.525%
        federal_long_term: None,
        niit: None,
        ordinary_income_cents: Cents(0),
        state: BracketState::Verified,
    };
    let mut states: BTreeMap<StateCode, ResolvedJurisdiction> = BTreeMap::new();
    states.insert("NJ".to_string(), nj);
    TaxContext {
        tax_year: TaxYear(tax_year),
        federal,
        states,
        de_minimis_cents: Cents(100),
        residency_default: Some("NJ".to_string()),
    }
}

/// The per-symbol `tax` unrealized estimates for a snapshot. `as_of` classifies
/// LT/ST.
pub fn estimates(
    snapshot: &Snapshot,
    as_of: Date,
    ctx: &TaxContext,
) -> BTreeMap<Symbol, UnrealizedEstimate> {
    let mut out: BTreeMap<Symbol, UnrealizedEstimate> = BTreeMap::new();
    for symbol in snapshot.positions.keys() {
        if snapshot.positions[symbol].total_qty.0 == 0 {
            continue;
        }
        let est = tax::unrealized_estimate(
            symbol,
            &snapshot.open_lots,
            as_of,
            &Some("NJ".to_string()),
            Cents(0),
            Cents(0),
            ctx,
        );
        out.insert(symbol.clone(), est);
    }
    out
}

/// The `tax` annual reserve rows for the small portfolio. Empty events → an annual
/// row per (jurisdiction, year) the realized gains touch.
pub fn annual_rows(snapshot: &Snapshot, ctx: &TaxContext) -> Vec<AnnualRow> {
    tax::annual_report(&snapshot.realized_gains, &[], ctx)
}

/// The `tax` annual reserve rows for the small portfolio WITH a `Move` and a `Pay`
/// for the federal `(year)` reserve, so `moved`/`paid` are non-zero and the summary
/// tax line's `moved` and `outstanding (accrued − paid)` figures are distinct from
/// `accrued`. `annual_report` sums Move/Pay by `(jurisdiction, tax_year)`, so a
/// minimal event pair drives the figures. (exercises the summary tax-line field
/// choice — accrued vs moved vs outstanding) Returns `(rows, moved_cents, paid_cents)`.
pub fn annual_rows_with_reserve_events(
    snapshot: &Snapshot,
    ctx: &TaxContext,
    year: i32,
    moved_cents: i64,
    paid_cents: i64,
) -> Vec<AnnualRow> {
    let key = AccrualKey {
        sale_id: "sale-1".to_string(),
        lot_id: "lot-goog".to_string(),
        jurisdiction: Jurisdiction::Federal,
        tax_year: TaxYear(year),
    };
    let events = vec![
        TaxEvent {
            seq: Seq(1),
            kind: TaxEventKind::Move {
                accrual_key: key.clone(),
                amount_cents: Cents(moved_cents),
                date: Date(19_110),
            },
        },
        TaxEvent {
            seq: Seq(2),
            kind: TaxEventKind::Pay {
                jurisdiction: Jurisdiction::Federal,
                tax_year: TaxYear(year),
                period: Quarter::Q2,
                amount_cents: Cents(paid_cents),
                date: Date(19_120),
                covers: vec![key],
            },
        },
    ];
    tax::annual_report(&snapshot.realized_gains, &events, ctx)
}

/// `SummaryInputs` like [`inputs_at`] but with non-zero `moved`/`paid` reserve events
/// folded into the annual rows, so the tax line shows distinct accrued/moved/outstanding.
pub fn inputs_with_reserve_events(
    key_day: i32,
    amzn_cents: i64,
    moved_cents: i64,
    paid_cents: i64,
) -> SummaryInputs {
    let mut inputs = inputs_at(key_day, amzn_cents);
    let snap = small_snapshot(&ledger_marks(&[("AMZN", amzn_cents), ("GOOG", 120_00)]));
    let c = ctx(2022);
    inputs.annual_rows = annual_rows_with_reserve_events(&snap, &c, 2022, moved_cents, paid_cents);
    inputs
}

// ---------------------------------------------------------------------------
// Series points + History rows.
// ---------------------------------------------------------------------------

/// Build a current-trading-day series point at `key_day` valuing AMZN at
/// `amzn_cents`/share and GOOG at $120/share (both priced).
pub fn point_at(key_day: i32, amzn_cents: i64) -> SeriesPoint {
    let marks = priced_marks(&[("AMZN", amzn_cents, key_day), ("GOOG", 120_00, key_day)]);
    let snap = small_snapshot(&ledger_marks(&[("AMZN", amzn_cents), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let c = ctx(2022);
    let est = estimates(&snap, as_of, &c);
    build_series_point(
        &snap,
        &marks,
        &est,
        TradingDayKey(Date(key_day)),
        1_700_000_000,
        Date(19_500),
    )
}

/// A well-formed History row for a point (checksum computed honestly).
pub fn row(point: &SeriesPoint) -> HistoryRow {
    HistoryRow {
        key: point.key,
        point: point.clone(),
        checksum: point_checksum(point),
    }
}

/// Mark a series point INCOMPLETE (a degraded capture) without changing its key —
/// for exercising suppressed-delta behavior. (REPORT-VOT-004)
pub fn incomplete(mut point: SeriesPoint) -> SeriesPoint {
    point.incomplete = true;
    point
}

// ---------------------------------------------------------------------------
// SummaryInputs builders.
// ---------------------------------------------------------------------------

/// `SummaryInputs` for the small portfolio at trading day `key_day`, valuing AMZN at
/// `amzn_cents`/share and GOOG at $120/share (both priced), Verified brackets,
/// tax year 2022.
pub fn inputs_at(key_day: i32, amzn_cents: i64) -> SummaryInputs {
    let marks = priced_marks(&[("AMZN", amzn_cents, key_day), ("GOOG", 120_00, key_day)]);
    let snap = small_snapshot(&ledger_marks(&[("AMZN", amzn_cents), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let c = ctx(2022);
    let est = estimates(&snap, as_of, &c);
    let rows = annual_rows(&snap, &c);
    SummaryInputs {
        snapshot: snap,
        estimates: est,
        annual_rows: rows,
        marks,
        trading_day_key: Some(TradingDayKey(Date(key_day))),
        trading_day_calendar: trading_calendar_around(key_day),
        reporting_tz_date: Date(19_500),
        run_at_epoch_secs: 1_700_000_000,
        bracket_state: BracketState::Verified,
        tax_year: 2022,
    }
}

/// A small trading-day calendar around `key_day`: the consecutive *trading* days
/// `key_day-5 ..= key_day` (a contiguous run, no weekend modelling). Used so the
/// gap computation has the calendar `runtime` supplies. (SUMMARY-DELTA-002)
pub fn trading_calendar_around(key_day: i32) -> Vec<TradingDayKey> {
    ((key_day - 5)..=key_day)
        .map(|d| TradingDayKey(Date(d)))
        .collect()
}

/// `SummaryInputs` for the small portfolio where **GOOG is unpriced** (degraded):
/// only AMZN carries a mark, so totals are partial and the trading-day key is
/// AMZN's quote-epoch. (SUMMARY-EMIT-003)
pub fn inputs_degraded(key_day: i32, amzn_cents: i64) -> SummaryInputs {
    let marks = priced_marks(&[("AMZN", amzn_cents, key_day)]); // GOOG omitted → degraded
    let snap = small_snapshot(&ledger_marks(&[("AMZN", amzn_cents)])); // GOOG unmarked
    let as_of = Date(19_500);
    let c = ctx(2022);
    let est = estimates(&snap, as_of, &c);
    let rows = annual_rows(&snap, &c);
    SummaryInputs {
        snapshot: snap,
        estimates: est,
        annual_rows: rows,
        marks,
        trading_day_key: Some(TradingDayKey(Date(key_day))),
        trading_day_calendar: trading_calendar_around(key_day),
        reporting_tz_date: Date(19_500),
        run_at_epoch_secs: 1_700_000_000,
        bracket_state: BracketState::Verified,
        tax_year: 2022,
    }
}

/// Override the bracket state on a `SummaryInputs` (Verified / Stale / cold-start).
pub fn with_bracket_state(mut inputs: SummaryInputs, state: BracketState) -> SummaryInputs {
    inputs.bracket_state = state;
    inputs
}
