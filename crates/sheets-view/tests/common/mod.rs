//! Shared test fixtures for the `sheets-view` suite: a small ledger replayed to a
//! `Snapshot`, a `TaxContext`, and the helpers that build accruals / reserves /
//! effective rates. No `#[test]`s here.
#![allow(dead_code)]
#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeMap;

use config::{BracketRow, BracketSet, BracketState, Jurisdiction, Niit, Ppm, StateCode, TaxYear};
use ledger_core::{LedgerEvent, LedgerEventKind, Marks, Snapshot, Symbol};
use pt_core::{Cents, Date, MicroShares, Seq};
use tax::{
    Accrual, AnnualRow, Reserve, ResolvedJurisdiction, TaxContext, TaxEvent, UnrealizedEstimate,
};

/// A `Buy` event opening a lot.
pub fn buy(
    seq: u64,
    date: i32,
    lot: &str,
    symbol: &str,
    qty_micro: i64,
    unit_cents: i64,
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
            platform: "schwab".to_string(),
            tracking_code: Some("TC-1".to_string()),
        },
    }
}

/// A `Sell` event (FIFO) disposing `qty_micro` shares of `symbol`.
pub fn sell(
    seq: u64,
    date: i32,
    sale: &str,
    symbol: &str,
    qty_micro: i64,
    unit_cents: i64,
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
            platform: "schwab".to_string(),
            tracking_code: None,
        },
    }
}

/// A marks map for replay (symbol → per-share Cents).
pub fn marks(entries: &[(&str, i64)]) -> Marks {
    entries
        .iter()
        .map(|(s, c)| (Symbol::from(*s), Cents(*c)))
        .collect()
}

/// Replay a log with marks into a `Snapshot`.
pub fn replay(events: &[LedgerEvent], marks: &Marks) -> Snapshot {
    ledger_core::replay(events, marks)
}

/// A small two-symbol portfolio: AMZN held (open), GOOG bought and partly sold.
/// Returns the replayed snapshot with the given marks.
pub fn small_snapshot(marks_in: &Marks) -> Snapshot {
    let events = vec![
        buy(1, 19_000, "lot-amzn", "AMZN", 3_000_000, 150_00),
        buy(2, 19_010, "lot-goog", "GOOG", 2_000_000, 100_00),
        sell(3, 19_100, "sale-1", "GOOG", 1_000_000, 130_00, Some("NJ")),
    ];
    replay(&events, marks_in)
}

// ---------------------------------------------------------------------------
// Tax context (one tax year, federal + NJ, simple flat brackets).
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

/// Compute the accruals for a snapshot's realized gains against a context (no
/// lifecycle events).
pub fn accruals(snapshot: &Snapshot, ctx: &TaxContext) -> Vec<Accrual> {
    let events: Vec<TaxEvent> = vec![];
    tax::compute_accruals(&snapshot.realized_gains, &events, ctx)
}

/// Compute the reserves for a snapshot's realized gains against a context.
pub fn reserves(snapshot: &Snapshot, ctx: &TaxContext) -> Vec<Reserve> {
    let events: Vec<TaxEvent> = vec![];
    tax::reserves(&snapshot.realized_gains, &events, ctx)
}

/// Compute the annual report rows for a snapshot's realized gains against a
/// context, as `sheets-view`'s Tax tab consumes them for the kernel-exact reserve
/// summary (accrued / moved / paid / outstanding / shortfall). (TAX-REPORT-003/004)
pub fn annual_rows(snapshot: &Snapshot, events: &[TaxEvent], ctx: &TaxContext) -> Vec<AnnualRow> {
    tax::annual_report(&snapshot.realized_gains, events, ctx)
}

/// Compute the per-position effective unrealized tax rates (symbol → Ppm) via
/// `tax::unrealized_estimate`, as `sheets-view` consumes them for the Positions
/// `Est. Tax Rate` column.
pub fn effective_rates(
    snapshot: &Snapshot,
    as_of: Date,
    ctx: &TaxContext,
) -> BTreeMap<Symbol, Ppm> {
    let mut out: BTreeMap<Symbol, Ppm> = BTreeMap::new();
    for symbol in snapshot.positions.keys() {
        let est: UnrealizedEstimate = tax::unrealized_estimate(
            symbol,
            &snapshot.open_lots,
            as_of,
            &Some("NJ".to_string()),
            Cents(0),
            Cents(0),
            ctx,
        );
        if let Some(rate) = est.effective_rate_ppm {
            out.insert(symbol.clone(), rate);
        }
    }
    out
}
