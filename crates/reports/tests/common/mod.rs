//! Shared test fixtures for the `reports` suite: small ledgers replayed to a
//! `Snapshot`, priced marks with quote-epochs, `tax` unrealized estimates, and
//! series-point / History-row builders. No `#[test]`s here.
#![allow(dead_code)]
#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeMap;

use config::{BracketRow, BracketSet, BracketState, Jurisdiction, Niit, Ppm, StateCode, TaxYear};
use ledger_core::{LedgerEvent, LedgerEventKind, Marks, Snapshot, Symbol};
use pt_core::{Cents, Date, MicroShares, Seq};
use reports::{PricedMark, PricedMarks};
use tax::{ResolvedJurisdiction, TaxContext, UnrealizedEstimate};

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

/// Replay a log with marks into a `Snapshot`.
pub fn replay(events: &[LedgerEvent], marks: &Marks) -> Snapshot {
    ledger_core::replay(events, marks)
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
    replay(&small_events(), marks)
}

// ---------------------------------------------------------------------------
// Tax context + unrealized estimates.
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

/// An empty ledger (no events at all → no positions).
pub fn small_events_empty() -> Vec<LedgerEvent> {
    Vec::new()
}

/// A synthetic snapshot carrying ONE position with a **negative** total basis —
/// not producible by the kernel (a Buy with a negative price is rejected), so it
/// is constructed directly to exercise the negative-basis flag. (REPORT-COMP-004)
pub fn negative_basis_snapshot() -> Snapshot {
    use ledger_core::{Lot, LotSource, OpenLot, Position};

    let mut positions: BTreeMap<Symbol, Position> = BTreeMap::new();
    positions.insert(
        "ACME".to_string(),
        Position {
            symbol: "ACME".to_string(),
            total_qty: MicroShares(1_000_000),
            total_basis_cents: Cents(-100_00), // negative basis (synthetic)
            realized_pnl_cents: Cents(0),
            unrealized_cents: Some(Cents(150_00)),
        },
    );
    let open_lots = vec![OpenLot {
        lot: Lot {
            id: "lot-acme".to_string(),
            symbol: "ACME".to_string(),
            acquire_date: Date(19_000),
            open_seq: Seq(1),
            source: LotSource::Buy,
            remaining_qty: MicroShares(1_000_000),
            remaining_basis_cents: Cents(-100_00),
            platform: "schwab".to_string(),
            tracking_code: None,
        },
        unrealized_cents: Some(Cents(150_00)),
    }];
    Snapshot {
        positions,
        open_lots,
        realized_gains: Vec::new(),
    }
}

/// A synthetic snapshot whose ONE platform ("schwab") holds two open lots of the
/// same symbol ("ACME") in an order that drives a running basis sum NEGATIVE on the
/// first lot (-$150) but POSITIVE on the aggregate (+$50) — so a platform whose
/// final aggregated basis is positive is never false-flagged negative by an order-
/// dependent intermediate check. Not kernel-producible (negative-basis lots),
/// constructed directly. (REPORT-COMP-004)
pub fn platform_running_negative_then_positive_snapshot() -> Snapshot {
    use ledger_core::{Lot, LotSource, OpenLot, Position};

    let lot = |id: &str, basis: i64| OpenLot {
        lot: Lot {
            id: id.to_string(),
            symbol: "ACME".to_string(),
            acquire_date: Date(19_000),
            open_seq: Seq(1),
            source: LotSource::Buy,
            remaining_qty: MicroShares(1_000_000),
            remaining_basis_cents: Cents(basis),
            platform: "schwab".to_string(),
            tracking_code: None,
        },
        unrealized_cents: None,
    };
    let mut positions: BTreeMap<Symbol, Position> = BTreeMap::new();
    positions.insert(
        "ACME".to_string(),
        Position {
            symbol: "ACME".to_string(),
            total_qty: MicroShares(2_000_000),
            total_basis_cents: Cents(50_00), // final aggregate basis is POSITIVE (+$50)
            realized_pnl_cents: Cents(0),
            unrealized_cents: None,
        },
    );
    // Lot A (-$150) precedes lot B (+$200): a running-sum check flips negative at A,
    // but the final aggregate (+$50) is positive.
    let open_lots = vec![lot("lot-a", -150_00), lot("lot-b", 200_00)];
    Snapshot {
        positions,
        open_lots,
        realized_gains: Vec::new(),
    }
}

/// A synthetic snapshot carrying ONE symbol ("ZZZ") held as several open lots
/// spread across TWO platforms, so the per-symbol exact estimated tax must be
/// distributed (largest-remainder) over its sub-positions by pre-tax weight and
/// the per-platform net-of-tax sub-totals must reconcile EXACTLY to the symbol's
/// estimated tax with no penny drift. Lot bases are chosen so the per-lot pretax
/// weights do NOT divide the tax evenly (forcing a residual unit). Constructed
/// directly (the kernel does not place multi-platform lots for one symbol from
/// these fixtures). (REPORT-COMP-005)
pub fn multi_lot_multi_platform_snapshot() -> Snapshot {
    use ledger_core::{Lot, LotSource, OpenLot, Position};

    let lot = |id: &str, platform: &str, qty: i64, basis: i64| OpenLot {
        lot: Lot {
            id: id.to_string(),
            symbol: "ZZZ".to_string(),
            acquire_date: Date(19_000),
            open_seq: Seq(1),
            source: LotSource::Buy,
            remaining_qty: MicroShares(qty),
            remaining_basis_cents: Cents(basis),
            platform: platform.to_string(),
            tracking_code: None,
        },
        unrealized_cents: None,
    };
    // Three lots of ZZZ: two on schwab, one on fidelity. Total 6 sh, total basis
    // $1000 (300 + 350 + 350). Quantities/bases chosen so per-lot pretax weights at
    // a $200/sh mark are 300/250/250 → the exact tax does not split evenly.
    let open_lots = vec![
        lot("lot-1", "schwab", 1_000_000, 300_00),
        lot("lot-2", "schwab", 2_000_000, 350_00),
        lot("lot-3", "fidelity", 3_000_000, 350_00),
    ];
    let mut positions: BTreeMap<Symbol, Position> = BTreeMap::new();
    positions.insert(
        "ZZZ".to_string(),
        Position {
            symbol: "ZZZ".to_string(),
            total_qty: MicroShares(6_000_000),
            total_basis_cents: Cents(1000_00),
            realized_pnl_cents: Cents(0),
            unrealized_cents: None,
        },
    );
    Snapshot {
        positions,
        open_lots,
        realized_gains: Vec::new(),
    }
}

/// An `UnrealizedEstimate` for one `symbol` with an explicit (possibly indivisible)
/// `estimated_tax_cents`, used to drive the largest-remainder apportionment of a
/// symbol's exact estimated tax across its lots/platforms. (REPORT-COMP-005)
pub fn fixed_estimate(symbol: &str, pretax_cents: i64, tax_cents: i64) -> UnrealizedEstimate {
    UnrealizedEstimate {
        symbol: symbol.to_string(),
        unrealized_pretax_cents: Cents(pretax_cents),
        estimated_tax_cents: Some(Cents(tax_cents)),
        effective_rate_ppm: Some(Ppm(0)),
        bracket_state: BracketState::Verified,
    }
}

/// The per-symbol `tax` unrealized estimates for a snapshot, as `reports`
/// consumes them for net-of-tax composition. `as_of` classifies LT/ST.
pub fn estimates(
    snapshot: &Snapshot,
    as_of: Date,
    ctx: &TaxContext,
) -> BTreeMap<Symbol, UnrealizedEstimate> {
    let mut out: BTreeMap<Symbol, UnrealizedEstimate> = BTreeMap::new();
    for symbol in snapshot.positions.keys() {
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
