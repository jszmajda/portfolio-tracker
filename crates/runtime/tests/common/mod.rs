//! Shared fixtures for the `runtime` test suite: ledger events, a `TaxContext`,
//! and event-log builders. No `#[test]`s here. Mirrors the kernels' test commons
//! so the cycle is driven with REAL ledger-core / tax / config types.
#![allow(dead_code)]
#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeMap;

use config::{BracketRow, BracketSet, BracketState, Jurisdiction, Niit, Ppm, StateCode, TaxYear};
use ledger_core::{LedgerEvent, LedgerEventKind};
use pt_core::{Cents, Date, MicroShares, Seq};
use store::EventLogs;
use tax::{ResolvedJurisdiction, TaxContext};

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

/// The small two-symbol event log: AMZN held (open), GOOG bought and partly sold
/// (a realized gain). No tax-lifecycle events (the cycle exercises replay + tax).
pub fn small_logs() -> EventLogs {
    EventLogs {
        ledger: vec![
            buy(1, 19_000, "lot-amzn", "AMZN", 3_000_000, 150_00),
            buy(2, 19_010, "lot-goog", "GOOG", 2_000_000, 100_00),
            sell(3, 19_100, "sale-1", "GOOG", 1_000_000, 130_00, Some("NJ")),
        ],
        tax: vec![],
    }
}

/// An event log in which GOOG is FULLY disposed (bought 2 shares, sold all 2), so
/// `ledger-core::replay` keeps GOOG in `positions` with `total_qty == 0` (via
/// `realized.keys()`). AMZN stays open. Used to prove the cycle does NOT price a
/// fully-closed symbol. (RUNTIME-CYCLE-002)
pub fn closed_symbol_logs() -> EventLogs {
    EventLogs {
        ledger: vec![
            buy(1, 19_000, "lot-amzn", "AMZN", 3_000_000, 150_00),
            buy(2, 19_010, "lot-goog", "GOOG", 2_000_000, 100_00),
            // Sell the ENTIRE GOOG position: 2 shares of 2 → qty 0, but GOOG remains
            // in positions via the realized gain.
            sell(3, 19_100, "sale-1", "GOOG", 2_000_000, 130_00, Some("NJ")),
        ],
        tax: vec![],
    }
}

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

/// A federal+NJ tax context for `tax_year`, with flat ordinary/LT/state rates
/// (mirrors the kernels' commons).
pub fn ctx(tax_year: i32) -> TaxContext {
    let federal = ResolvedJurisdiction {
        jurisdiction: Jurisdiction::Federal,
        ordinary: Some(flat_set(370_000)),
        federal_long_term: Some(flat_set(200_000)),
        niit: Some(Niit {
            rate_ppm: Ppm(38_000),
            magi_threshold_cents: Cents(0),
        }),
        ordinary_income_cents: Cents(0),
        state: BracketState::Verified,
    };
    let nj = ResolvedJurisdiction {
        jurisdiction: Jurisdiction::State("NJ".to_string()),
        ordinary: Some(flat_set(55_250)),
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
