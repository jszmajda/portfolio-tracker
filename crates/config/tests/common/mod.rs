//! Shared construction helpers for the `config` RED-phase tests. Keeps each
//! test a minimal, readable scenario against the real public API. All amounts
//! are exact integers (`Cents` / `Ppm`); no float ever appears.

#![allow(dead_code)]
// Cents literals are grouped as dollars + the trailing cents pair (e.g.
// `1_000_000_00` reads "$1,000,000.00"), which is intentional and clearer than
// a flat 3-digit grouping for money. clippy's grouping lint is advisory here.
#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeMap;

use config::{
    AliasMap, BracketRow, BracketSet, DeMinimis, FilingStatus, Niit, PlatformList, ResidencyEntry,
    StateCode, TaxRules, TaxYear,
};
use pt_core::{Cents, Date};

/// A bracket row from `(threshold_cents, rate_ppm)`.
pub fn row(threshold_cents: i64, rate_ppm: i64) -> BracketRow {
    BracketRow {
        lower_threshold_cents: Cents(threshold_cents),
        rate_ppm: config::Ppm(rate_ppm),
    }
}

/// A bracket set from `(threshold, rate)` pairs with a `last_verified` date.
pub fn bracket_set(rows: &[(i64, i64)], last_verified: i32) -> BracketSet {
    BracketSet {
        rows: rows.iter().map(|&(t, r)| row(t, r)).collect(),
        last_verified: Date(last_verified),
        source_note: "test source".to_string(),
    }
}

/// A flat single-row `[(0, rate)]` bracket set.
pub fn flat(rate_ppm: i64, last_verified: i32) -> BracketSet {
    bracket_set(&[(0, rate_ppm)], last_verified)
}

/// NIIT from `(rate_ppm, magi_threshold_cents)`.
pub fn niit(rate_ppm: i64, magi_threshold_cents: i64) -> Niit {
    Niit {
        rate_ppm: config::Ppm(rate_ppm),
        magi_threshold_cents: Cents(magi_threshold_cents),
    }
}

/// A residency entry from `(effective_date_days, state)`.
pub fn res(effective_date: i32, state: &str) -> ResidencyEntry {
    ResidencyEntry {
        effective_date: Date(effective_date),
        state_code: state.to_string(),
    }
}

/// A full per-year `TaxRules` from the given pieces. `states` is a list of
/// `(state_code, BracketSet)`.
pub fn rules(
    tax_year: i32,
    federal_ordinary: BracketSet,
    federal_long_term: BracketSet,
    niit: Niit,
    states: &[(&str, BracketSet)],
    ordinary_income_cents: i64,
) -> TaxRules {
    let mut state_ordinary: BTreeMap<StateCode, BracketSet> = BTreeMap::new();
    for (s, set) in states {
        state_ordinary.insert((*s).to_string(), set.clone());
    }
    TaxRules {
        tax_year: TaxYear(tax_year),
        filing_status: FilingStatus::MarriedFilingJointly,
        federal_ordinary,
        federal_long_term,
        niit,
        state_ordinary,
        ordinary_income_cents: Cents(ordinary_income_cents),
    }
}

/// A minimal, valid `TaxRules` for `tax_year`: modest fed-ordinary/LT brackets,
/// NIIT 3.8%, a DC and NJ state set, verified on `last_verified`. The stacked
/// rate stays well below 100%.
pub fn valid_rules(tax_year: i32, last_verified: i32) -> TaxRules {
    rules(
        tax_year,
        // federal ordinary: 10% then 37% top
        bracket_set(&[(0, 100_000), (1_000_000_00, 370_000)], last_verified),
        // federal long-term: 0 / 15 / 20%
        bracket_set(
            &[(0, 0), (500_000_00, 150_000), (5_000_000_00, 200_000)],
            last_verified,
        ),
        niit(38_000, 250_000_00),
        &[
            // DC top ~10.75%
            ("DC", bracket_set(&[(0, 40_000), (10_000_000_00, 107_500)], last_verified)),
            // NJ top 5.525% (the rate bps cannot represent)
            ("NJ", bracket_set(&[(0, 14_000), (1_000_000_00, 55_250)], last_verified)),
        ],
        300_000_00,
    )
}

/// A few platform names.
pub fn platforms() -> PlatformList {
    PlatformList::new(vec!["schwab".to_string(), "fidelity".to_string()])
}

/// A small alias map (e.g. `BRK` → `BRK.B`).
pub fn aliases() -> AliasMap {
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    m.insert("BRK".to_string(), "BRK.B".to_string());
    AliasMap::new(m)
}

/// The de-minimis $1 (`100` cents).
pub fn de_minimis_dollar() -> DeMinimis {
    DeMinimis(Cents(100))
}
