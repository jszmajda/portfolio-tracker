//! RED-phase TDD tests for the CONFIG-STALE-* specs of `config` (see
//! `docs/intent/config/config-specs.md` → "Resolution, Degradation & Staleness").
//!
//! These exercise the per-jurisdiction degrade-to-prior-year resolution (flagged
//! `Stale`), the cold-start `NoBracketsAvailable` signal, the staleness
//! threshold check, the residency-derived active set (computed from config's OWN
//! data, never `tax` accrual state), and the surfaced `(jurisdiction, year)`
//! signals. The scaffold stubs the resolution bodies with `unimplemented!()`, so
//! each test PANICS (RED).

mod common;

use std::collections::BTreeMap;

use common::*;
use config::{
    active_jurisdictions, is_stale, resolve_brackets, staleness_signals, BracketState,
    Jurisdiction, ResidencyTimeline, StalenessSignal, TaxRules, TaxYear,
};
use pt_core::Date;

fn series(years: &[(i32, i32)]) -> BTreeMap<TaxYear, TaxRules> {
    // (tax_year, last_verified_days) -> a valid rule table for that year.
    let mut m = BTreeMap::new();
    for &(y, lv) in years {
        m.insert(TaxYear(y), valid_rules(y, lv));
    }
    m
}

// @spec CONFIG-STALE-001
#[test]
fn resolve_returns_verified_set_for_the_requested_year() {
    let s = series(&[(2024, 19_300), (2025, 19_700)]);
    let r = resolve_brackets(&s, &Jurisdiction::Federal, TaxYear(2025));
    assert_eq!(r.state, BracketState::Verified);
    assert!(r.set.is_some());
    // It is the 2025 set (last_verified 19_700), not a degraded prior set.
    assert_eq!(r.set.unwrap().last_verified, Date(19_700));
}

// @spec CONFIG-STALE-001
#[test]
fn resolve_degrades_to_most_recent_prior_year_flagged_stale() {
    // No 2025 set; the most recent prior set is 2023 (skip-gap over 2024 absent).
    let s = series(&[(2022, 19_000), (2023, 19_300)]);
    let r = resolve_brackets(&s, &Jurisdiction::Federal, TaxYear(2025));
    assert_eq!(r.state, BracketState::Stale);
    // Degraded to 2023 (the greatest year < 2025 carrying a set), not 2022.
    assert_eq!(r.set.expect("a degraded set").last_verified, Date(19_300));
}

// @spec CONFIG-STALE-001
#[test]
fn resolve_cold_start_returns_no_brackets_available() {
    // No set in any year <= requested for this jurisdiction.
    let s: BTreeMap<TaxYear, TaxRules> = BTreeMap::new();
    let r = resolve_brackets(&s, &Jurisdiction::Federal, TaxYear(2025));
    assert_eq!(r.state, BracketState::NoBracketsAvailable);
    assert!(r.set.is_none());
}

// @spec CONFIG-STALE-001
#[test]
fn resolve_is_independent_per_jurisdiction() {
    // Federal has a 2025 set; a newly-added state (MD) has none in any year.
    let s = series(&[(2025, 19_700)]); // valid_rules seeds DC + NJ, not MD.
    let fed = resolve_brackets(&s, &Jurisdiction::Federal, TaxYear(2025));
    let md = resolve_brackets(&s, &Jurisdiction::State("MD".to_string()), TaxYear(2025));
    assert_eq!(fed.state, BracketState::Verified);
    // MD is cold-start even though Federal is verified — resolution is per-jurisdiction.
    assert_eq!(md.state, BracketState::NoBracketsAvailable);
    // DC, present in the seed, resolves verified.
    let dc = resolve_brackets(&s, &Jurisdiction::State("DC".to_string()), TaxYear(2025));
    assert_eq!(dc.state, BracketState::Verified);
}

// @spec CONFIG-STALE-002
#[test]
fn jurisdiction_with_no_set_for_current_year_is_stale() {
    // Federal verified only for 2023; current year is 2025 → stale.
    let s = series(&[(2023, 19_300)]);
    assert!(is_stale(
        &s,
        &Jurisdiction::Federal,
        TaxYear(2025),
        Date(20_300),
        12,
    ));
}

// @spec CONFIG-STALE-002
#[test]
fn fresh_set_within_threshold_is_not_stale_but_old_set_is() {
    // A 2025 set verified ~1 month before as_of (within 12 months) → not stale.
    // Day 20_240 verified, as_of 20_270 (~30 days later).
    let s = series(&[(2025, 20_240)]);
    assert!(!is_stale(
        &s,
        &Jurisdiction::Federal,
        TaxYear(2025),
        Date(20_270),
        12,
    ));

    // A 2025 set verified > 12 months before as_of → stale by age.
    // 12 months ~= 365 days; verified 18_000, as_of 19_000 (1000 days later).
    let s_old = series(&[(2025, 18_000)]);
    assert!(is_stale(
        &s_old,
        &Jurisdiction::Federal,
        TaxYear(2025),
        Date(19_000),
        12,
    ));
}

// @spec CONFIG-STALE-003
#[test]
fn active_set_is_federal_plus_residency_states_in_lookback_window() {
    // Residency: NJ effective 2022-01-08, DC effective 2025-01-12.
    // current_year 2025, lookback 1 → window [2024-01-01, 2025-12-31].
    // 19_000 = 2022-01-08, 20_100 = 2025-01-12.
    let tl =
        ResidencyTimeline::from_entries(vec![res(19_000, "NJ"), res(20_100, "DC")]).expect("valid");
    let active = active_jurisdictions(&tl, TaxYear(2025), 1);

    // Federal is always active.
    assert!(active.contains(&Jurisdiction::Federal));
    // DC (effective in-window) is active.
    assert!(active.contains(&Jurisdiction::State("DC".to_string())));
    // NJ is the residency in effect during 2024 (within the window) even though
    // the owner moved to DC in 2025 — over-include a recently-left state safely.
    assert!(active.contains(&Jurisdiction::State("NJ".to_string())));
}

// @spec CONFIG-STALE-003
#[test]
fn active_set_excludes_not_yet_effective_future_entries() {
    // current_year 2025; a future move to NY effective 2029-01-01 is excluded.
    // 20_100 = 2025-01-12, 21_550 = 2029-01-01 (well past the window's end).
    let tl =
        ResidencyTimeline::from_entries(vec![res(20_100, "DC"), res(21_550, "NY")]).expect("valid");
    let active = active_jurisdictions(&tl, TaxYear(2025), 1);
    assert!(active.contains(&Jurisdiction::State("DC".to_string())));
    // NY's entry is not yet effective as of end-of-2025 → excluded.
    assert!(!active.contains(&Jurisdiction::State("NY".to_string())));
}

// @spec CONFIG-STALE-004
#[test]
fn surfaces_one_signal_per_stale_active_jurisdiction_for_current_year() {
    // Federal + DC have 2025 sets verified long ago (stale by age); NJ has none.
    // valid_rules seeds DC and NJ. Use last_verified far in the past (day 0).
    let s = series(&[(2025, 0)]); // verified at epoch → very stale vs as_of.
                                  // Residency keeps DC and NJ in the active window (current + prior year).
                                  // 20_000 = 2024-10-04, 20_100 = 2025-01-12.
    let tl =
        ResidencyTimeline::from_entries(vec![res(20_000, "NJ"), res(20_100, "DC")]).expect("valid");

    let signals = staleness_signals(&s, &tl, TaxYear(2025), Date(20_300), 12, 1);

    // Federal is stale (verified at epoch, well over 12 months before as_of).
    assert!(signals.contains(&StalenessSignal {
        jurisdiction: Jurisdiction::Federal,
        tax_year: TaxYear(2025),
    }));
    // DC is in the active set and its 2025 set is stale by age.
    assert!(signals.contains(&StalenessSignal {
        jurisdiction: Jurisdiction::State("DC".to_string()),
        tax_year: TaxYear(2025),
    }));
    // NJ is in the active set and has no 2025 set → stale.
    assert!(signals.contains(&StalenessSignal {
        jurisdiction: Jurisdiction::State("NJ".to_string()),
        tax_year: TaxYear(2025),
    }));
}

// @spec CONFIG-STALE-004
#[test]
fn no_signals_when_all_active_jurisdictions_are_fresh() {
    // All sets verified just before as_of (well within 12 months).
    let s = series(&[(2025, 20_270)]);
    let tl = ResidencyTimeline::from_entries(vec![res(20_100, "DC")]).expect("valid");
    // DC is active and fresh; NJ is not in the residency window → not checked.
    let signals = staleness_signals(&s, &tl, TaxYear(2025), Date(20_300), 12, 1);
    assert!(signals.is_empty());
}
