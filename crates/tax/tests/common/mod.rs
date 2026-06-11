//! Shared construction helpers for the `tax` RED-phase tests. Keeps each test a
//! minimal, readable scenario against the real public API. All amounts are exact
//! integers (`Cents` / `Ppm`); no float ever appears. Dates are days since the
//! Unix epoch (matching `pt_core::Date`), built from civil `(y, m, d)` via the
//! same Howard-Hinnant algorithm `config` uses, so expectations read as dates.

#![allow(dead_code)]
// Cents literals are grouped as dollars + the trailing cents pair (e.g.
// `1_000_000_00` reads "$1,000,000.00"); clippy's grouping lint is advisory.
#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeMap;

use config::{
    BracketRow, BracketSet, BracketState, Jurisdiction, Niit, Ppm, StateCode, TaxYear,
};
use ledger_core::{Lot, LotSource, OpenLot, RealizedGain, SaleId, Symbol};
use pt_core::{Cents, Date, MicroShares, Seq};
use tax::{
    AccrualKey, ResolvedJurisdiction, TaxContext, TaxEvent, TaxEventKind,
};

/// Days since 1970-01-01 for civil `(y, m, d)` (proleptic Gregorian) — the same
/// algorithm `config` and the kernel's date math use.
pub fn day(y: i32, m: i32, d: i32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + (d - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era as i64 * 146097 + doe - 719468) as i32
}

/// A `Date` for civil `(y, m, d)`.
pub fn date(y: i32, m: i32, d: i32) -> Date {
    Date(day(y, m, d))
}

/// One whole share in MicroShares.
pub const SHARE: i64 = 1_000_000;

// ---------------------------------------------------------------------------
// Bracket / context construction.
// ---------------------------------------------------------------------------

pub fn row(threshold_cents: i64, rate_ppm: i64) -> BracketRow {
    BracketRow {
        lower_threshold_cents: Cents(threshold_cents),
        rate_ppm: Ppm(rate_ppm),
    }
}

pub fn bracket_set(rows: &[(i64, i64)]) -> BracketSet {
    BracketSet {
        rows: rows.iter().map(|&(t, r)| row(t, r)).collect(),
        last_verified: date(2025, 1, 1),
        source_note: "test".to_string(),
    }
}

/// A flat single-rate bracket set `[(0, rate)]`.
pub fn flat(rate_ppm: i64) -> BracketSet {
    bracket_set(&[(0, rate_ppm)])
}

pub fn niit(rate_ppm: i64, magi_threshold_cents: i64) -> Niit {
    Niit {
        rate_ppm: Ppm(rate_ppm),
        magi_threshold_cents: Cents(magi_threshold_cents),
    }
}

/// A federal `ResolvedJurisdiction` with the given ordinary set, LT set, NIIT,
/// income, and `BracketState`.
pub fn federal(
    ordinary: BracketSet,
    federal_long_term: BracketSet,
    niit: Niit,
    ordinary_income_cents: i64,
    state: BracketState,
) -> ResolvedJurisdiction {
    ResolvedJurisdiction {
        jurisdiction: Jurisdiction::Federal,
        ordinary: Some(ordinary),
        federal_long_term: Some(federal_long_term),
        niit: Some(niit),
        ordinary_income_cents: Cents(ordinary_income_cents),
        state,
    }
}

/// A state `ResolvedJurisdiction` taxing all gains at its ordinary set.
pub fn state(code: &str, ordinary: BracketSet, income_cents: i64, st: BracketState) -> ResolvedJurisdiction {
    ResolvedJurisdiction {
        jurisdiction: Jurisdiction::State(code.to_string()),
        ordinary: Some(ordinary),
        federal_long_term: None,
        niit: None,
        ordinary_income_cents: Cents(income_cents),
        state: st,
    }
}

/// A `NoBracketsAvailable` state jurisdiction (no resolved set).
pub fn state_no_brackets(code: &str) -> ResolvedJurisdiction {
    ResolvedJurisdiction {
        jurisdiction: Jurisdiction::State(code.to_string()),
        ordinary: None,
        federal_long_term: None,
        niit: None,
        ordinary_income_cents: Cents(0),
        state: BracketState::NoBracketsAvailable,
    }
}

/// A `TaxContext` for `tax_year` with a federal jurisdiction, a list of state
/// jurisdictions, a de-minimis threshold, and an optional residency default.
pub fn context(
    tax_year: i32,
    federal: ResolvedJurisdiction,
    states: &[ResolvedJurisdiction],
    de_minimis_cents: i64,
    residency_default: Option<&str>,
) -> TaxContext {
    let mut map: BTreeMap<StateCode, ResolvedJurisdiction> = BTreeMap::new();
    for s in states {
        if let Jurisdiction::State(code) = &s.jurisdiction {
            map.insert(code.clone(), s.clone());
        }
    }
    TaxContext {
        tax_year: TaxYear(tax_year),
        federal,
        states: map,
        de_minimis_cents: Cents(de_minimis_cents),
        residency_default: residency_default.map(|s| s.to_string()),
    }
}

/// A simple federal-only context for `tax_year`: flat 20% ordinary, flat 15% LT,
/// no NIIT, zero income (so the marginal band sits at the bottom). De-minimis $0.
pub fn simple_federal_context(tax_year: i32) -> TaxContext {
    context(
        tax_year,
        federal(flat(200_000), flat(150_000), niit(0, 0), 0, BracketState::Verified),
        &[],
        0,
        None,
    )
}

// ---------------------------------------------------------------------------
// RealizedGain construction.
// ---------------------------------------------------------------------------

/// A `RealizedGain` from its identity, dates, and gain. `proceeds`/`basis` are
/// derived as `(gain, 0)` for positive gains (the exact split is immaterial to
/// `tax`, which reads `gain_cents` and the dates).
#[allow(clippy::too_many_arguments)]
pub fn gain(
    sale_id: &str,
    sale_seq: u64,
    lot_id: &str,
    symbol: &str,
    acquire: Date,
    sale: Date,
    gain_cents: i64,
    accrues_to_state: Option<&str>,
) -> RealizedGain {
    RealizedGain {
        sale_id: sale_id.to_string(),
        sale_seq: Seq(sale_seq),
        lot_id: lot_id.to_string(),
        symbol: symbol.to_string(),
        sale_date: sale,
        proceeds_cents: Cents(gain_cents.max(0)),
        basis_cents: Cents(0),
        gain_cents: Cents(gain_cents),
        acquire_date: acquire,
        holding_days: sale.0 - acquire.0,
        accrues_to_state: accrues_to_state.map(|s| s.to_string()),
    }
}

/// An `OpenLot` with a per-lot unrealized value (or `None` when degraded).
pub fn open_lot(
    lot_id: &str,
    symbol: &str,
    acquire: Date,
    qty: i64,
    basis_cents: i64,
    unrealized_cents: Option<i64>,
) -> OpenLot {
    OpenLot {
        lot: Lot {
            id: lot_id.to_string(),
            symbol: symbol.to_string(),
            acquire_date: acquire,
            open_seq: Seq(1),
            source: LotSource::Buy,
            remaining_qty: MicroShares(qty),
            remaining_basis_cents: Cents(basis_cents),
            platform: "fidelity".to_string(),
            tracking_code: None,
        },
        unrealized_cents: unrealized_cents.map(Cents),
    }
}

// ---------------------------------------------------------------------------
// AccrualKey & TaxEvent construction.
// ---------------------------------------------------------------------------

pub fn fed_key(sale_id: &str, lot_id: &str, tax_year: i32) -> AccrualKey {
    AccrualKey {
        sale_id: sale_id.to_string(),
        lot_id: lot_id.to_string(),
        jurisdiction: Jurisdiction::Federal,
        tax_year: TaxYear(tax_year),
    }
}

pub fn state_key(sale_id: &str, lot_id: &str, code: &str, tax_year: i32) -> AccrualKey {
    AccrualKey {
        sale_id: sale_id.to_string(),
        lot_id: lot_id.to_string(),
        jurisdiction: Jurisdiction::State(code.to_string()),
        tax_year: TaxYear(tax_year),
    }
}

pub fn allocate(seq: u64, key: AccrualKey, label: &str) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::Allocate {
            accrual_key: key,
            account_label: label.to_string(),
        },
    }
}

pub fn move_(seq: u64, key: AccrualKey, amount_cents: i64, d: Date) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::Move {
            accrual_key: key,
            amount_cents: Cents(amount_cents),
            date: d,
        },
    }
}

pub fn pay(
    seq: u64,
    jurisdiction: Jurisdiction,
    tax_year: i32,
    period: tax::Quarter,
    amount_cents: i64,
    d: Date,
    covers: Vec<AccrualKey>,
) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::Pay {
            jurisdiction,
            tax_year: TaxYear(tax_year),
            period,
            amount_cents: Cents(amount_cents),
            date: d,
            covers,
        },
    }
}

pub fn override_(seq: u64, key: AccrualKey, applied_amount_cents: i64, reason: &str) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::AmountOverride {
            accrual_key: key,
            applied_amount_cents: Cents(applied_amount_cents),
            reason: reason.to_string(),
        },
    }
}

pub fn seed_migration(
    seq: u64,
    jurisdiction: Jurisdiction,
    tax_year: i32,
    applied_amount_cents: i64,
    reason: &str,
) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::SeedMigration {
            jurisdiction,
            tax_year: TaxYear(tax_year),
            applied_amount_cents: Cents(applied_amount_cents),
            reason: reason.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Accrual lookup helpers.
// ---------------------------------------------------------------------------

/// Find the accrual for `(sale_id, lot_id, jurisdiction)` in a computed set.
pub fn find_accrual<'a>(
    accruals: &'a [tax::Accrual],
    sale_id: &str,
    lot_id: &str,
    jurisdiction: &Jurisdiction,
) -> Option<&'a tax::Accrual> {
    accruals.iter().find(|a| {
        a.key.sale_id == sale_id
            && a.key.lot_id == lot_id
            && a.key.jurisdiction == *jurisdiction
    })
}

/// All federal accruals in the computed set.
pub fn fed_accruals(accruals: &[tax::Accrual]) -> Vec<&tax::Accrual> {
    accruals
        .iter()
        .filter(|a| a.key.jurisdiction == Jurisdiction::Federal)
        .collect()
}

pub fn symbol_str(s: &str) -> Symbol {
    s.to_string()
}

pub fn sale_id_str(s: &str) -> SaleId {
    s.to_string()
}
