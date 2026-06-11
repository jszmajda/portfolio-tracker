//! RED-phase TDD tests for the CONFIG-VALID-* specs of `config` (see
//! `docs/intent/config/config-specs.md` → "Validation").
//!
//! These exercise the bracket-set shape gate (non-empty, strictly-ascending
//! unique thresholds, first row at 0, non-negative rates), the STACKED-rate
//! <100% ceiling that gates `tax`'s bounded-tax invariant, the non-negativity
//! of NIIT / de-minimis / income, and the import founding-residency
//! precondition. The scaffold stubs the validation bodies with
//! `unimplemented!()`, so each test PANICS (RED). A rejected write must leave the
//! stored config untouched.
//!
//! Cents literals are grouped as dollars + trailing cents (e.g. `1_000_000_00`
//! reads "$1,000,000.00"); clippy's grouping lint is advisory here.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use config::{
    check_import_founding_residency, federal_baseline_top_rate_ppm, stacked_top_rate_ppm,
    validate_bracket_set, validate_tax_rules, BracketSet, ConfigError, ConfigStore, DeMinimis,
    InMemoryConfig, Niit, ResidencyTimeline, TaxYear, PPM_FULL,
};
use pt_core::{Cents, Date};

// @spec CONFIG-VALID-001
#[test]
fn valid_bracket_set_is_accepted_including_single_flat_rate() {
    // A multi-row ascending set is valid.
    assert_eq!(
        validate_bracket_set(&bracket_set(&[(0, 100_000), (1_000_000_00, 370_000)], 19_700)),
        Ok(()),
    );
    // A single flat-rate [(0, r)] set is valid.
    assert_eq!(validate_bracket_set(&flat(50_000, 19_700)), Ok(()));
}

// @spec CONFIG-VALID-001
#[test]
fn empty_bracket_set_is_rejected() {
    let empty = BracketSet {
        rows: vec![],
        last_verified: Date(19_700),
        source_note: "x".to_string(),
    };
    assert_eq!(
        validate_bracket_set(&empty),
        Err(ConfigError::EmptyBracketSet),
    );
}

// @spec CONFIG-VALID-001
#[test]
fn first_row_not_at_zero_is_rejected() {
    assert_eq!(
        validate_bracket_set(&bracket_set(&[(100, 100_000)], 19_700)),
        Err(ConfigError::FirstRowNotZero),
    );
}

// @spec CONFIG-VALID-001
#[test]
fn non_ascending_or_duplicate_thresholds_are_rejected() {
    // Duplicate threshold.
    assert_eq!(
        validate_bracket_set(&bracket_set(&[(0, 100_000), (0, 200_000)], 19_700)),
        Err(ConfigError::ThresholdsNotStrictlyAscending),
    );
    // Descending threshold.
    assert_eq!(
        validate_bracket_set(&bracket_set(&[(0, 100_000), (5_000, 200_000), (1_000, 300_000)], 19_700)),
        Err(ConfigError::ThresholdsNotStrictlyAscending),
    );
}

// @spec CONFIG-VALID-001
#[test]
fn negative_rate_row_is_rejected() {
    assert_eq!(
        validate_bracket_set(&bracket_set(&[(0, -1)], 19_700)),
        Err(ConfigError::NegativeRate),
    );
}

// @spec CONFIG-VALID-002
#[test]
fn stacked_top_rate_sums_fed_max_plus_state_plus_niit() {
    // fed-ordinary top 370_000, fed-LT top 200_000 → max 370_000.
    // NJ top 55_250, NIIT 38_000 → stacked 370_000 + 55_250 + 38_000 = 463_250.
    let r = valid_rules(2025, 19_700);
    assert_eq!(stacked_top_rate_ppm(&r, &"NJ".to_string()), 463_250);
    // DC top 107_500 → 370_000 + 107_500 + 38_000 = 515_500.
    assert_eq!(stacked_top_rate_ppm(&r, &"DC".to_string()), 515_500);
}

// @spec CONFIG-VALID-002
#[test]
fn stacked_rate_reaching_100_percent_is_rejected() {
    // Construct a state whose top + fed-max + NIIT reaches 1_000_000 ppm exactly.
    // fed-ordinary top 370_000, fed-LT top 200_000 → max 370_000; NIIT 38_000.
    // Need state top = 1_000_000 - 370_000 - 38_000 = 592_000 to REACH 100%.
    let mut r = valid_rules(2025, 19_700);
    r.state_ordinary.insert(
        "XX".to_string(),
        bracket_set(&[(0, 100_000), (1_000_000_00, 592_000)], 19_700),
    );
    assert_eq!(stacked_top_rate_ppm(&r, &"XX".to_string()), PPM_FULL);
    assert_eq!(
        validate_tax_rules(&r),
        Err(ConfigError::StackedRateExceedsCeiling),
    );
}

// @spec CONFIG-VALID-002
#[test]
fn stacked_rate_just_below_100_percent_is_accepted() {
    // One ppm below the ceiling: state top 591_999 → stacked 999_999 < 1_000_000.
    let mut r = valid_rules(2025, 19_700);
    r.state_ordinary.insert(
        "XX".to_string(),
        bracket_set(&[(0, 100_000), (1_000_000_00, 591_999)], 19_700),
    );
    assert_eq!(stacked_top_rate_ppm(&r, &"XX".to_string()), PPM_FULL - 1);
    assert_eq!(validate_tax_rules(&r), Ok(()));
}

// @spec CONFIG-VALID-002
#[test]
fn the_baseline_seed_rules_pass_the_stacked_gate() {
    // The realistic MFJ seed (fed 37%, LT 20%, DC 10.75%, NJ 5.525%, NIIT 3.8%)
    // is well below 100% stacked and must validate.
    assert_eq!(validate_tax_rules(&valid_rules(2025, 19_700)), Ok(()));
}

// @spec CONFIG-VALID-002, CONFIG-VALID-005
#[test]
fn stateless_year_with_federal_plus_niit_reaching_100_percent_is_rejected() {
    // A federal-only tax year (no state bracket sets) must STILL be gated: the
    // bounded-tax breach is `max(fed-ord top, fed-LT top) + NIIT >= 100%` with no
    // state term. fed-LT top 962_000 + NIIT 38_000 = 1_000_000 ppm REACHES 100%.
    let mut r = valid_rules(2025, 19_700);
    r.state_ordinary.clear();
    assert!(r.state_ordinary.is_empty());
    // Raise fed-LT top so fed-max + NIIT reaches exactly the ceiling.
    r.federal_long_term =
        bracket_set(&[(0, 0), (5_000_000_00, 962_000)], 19_700);
    assert_eq!(federal_baseline_top_rate_ppm(&r), PPM_FULL);
    assert_eq!(
        validate_tax_rules(&r),
        Err(ConfigError::StackedRateExceedsCeiling),
    );
}

// @spec CONFIG-VALID-002, CONFIG-VALID-005
#[test]
fn stateless_year_just_below_100_percent_is_accepted() {
    // One ppm below: fed-LT top 961_999 + NIIT 38_000 = 999_999 < 1_000_000.
    let mut r = valid_rules(2025, 19_700);
    r.state_ordinary.clear();
    r.federal_long_term =
        bracket_set(&[(0, 0), (5_000_000_00, 961_999)], 19_700);
    assert_eq!(federal_baseline_top_rate_ppm(&r), PPM_FULL - 1);
    assert_eq!(validate_tax_rules(&r), Ok(()));
}

// @spec CONFIG-VALID-003
#[test]
fn negative_niit_income_or_de_minimis_is_rejected() {
    // Negative NIIT rate.
    let mut r = valid_rules(2025, 19_700);
    r.niit = Niit {
        rate_ppm: config::Ppm(-1),
        magi_threshold_cents: Cents(250_000_00),
    };
    assert_eq!(validate_tax_rules(&r), Err(ConfigError::NegativeAmount));

    // Negative annual income.
    let mut r2 = valid_rules(2025, 19_700);
    r2.ordinary_income_cents = Cents(-1);
    assert_eq!(validate_tax_rules(&r2), Err(ConfigError::NegativeAmount));

    // Negative NIIT MAGI threshold.
    let mut r3 = valid_rules(2025, 19_700);
    r3.niit = Niit {
        rate_ppm: config::Ppm(38_000),
        magi_threshold_cents: Cents(-1),
    };
    assert_eq!(validate_tax_rules(&r3), Err(ConfigError::NegativeAmount));

    // Negative de-minimis is refused at the write boundary.
    let mut store = InMemoryConfig::cold_start();
    assert_eq!(
        store.put_de_minimis(DeMinimis(Cents(-1)), &pt_core::NoopLock::new()),
        Err(ConfigError::NegativeAmount),
    );
}

// @spec CONFIG-VALID-001
#[test]
fn invalid_write_leaves_stored_config_untouched() {
    let mut store = InMemoryConfig::cold_start();
    store.put_tax_rules(valid_rules(2025, 19_700), &pt_core::NoopLock::new()).expect("seed");

    // An invalid write (empty federal-ordinary set) is refused...
    let mut bad = valid_rules(2025, 19_700);
    bad.federal_ordinary = BracketSet {
        rows: vec![],
        last_verified: Date(19_700),
        source_note: "x".to_string(),
    };
    assert_eq!(
        store.put_tax_rules(bad, &pt_core::NoopLock::new()),
        Err(ConfigError::EmptyBracketSet),
    );

    // ...and the previously-stored 2025 table is untouched (still valid).
    let data = store.load().expect("load");
    let kept = data.rules_by_year.get(&TaxYear(2025)).expect("kept");
    assert!(!kept.federal_ordinary.rows.is_empty());
}

// @spec CONFIG-VALID-004
#[test]
fn import_is_blocked_without_a_founding_residency_entry() {
    // No entry at or before the earliest event date → import blocked.
    let tl = ResidencyTimeline::from_entries(vec![res(19_000, "DC")]).expect("valid");
    assert_eq!(
        check_import_founding_residency(&tl, Date(18_000)),
        Err(ConfigError::MissingFoundingResidency),
    );
    // An empty timeline also blocks import.
    assert_eq!(
        check_import_founding_residency(&ResidencyTimeline::new(), Date(18_000)),
        Err(ConfigError::MissingFoundingResidency),
    );
}

// @spec CONFIG-VALID-004
#[test]
fn import_proceeds_once_a_founding_entry_exists_at_or_before_earliest_event() {
    // A founding entry exactly at the earliest event date suffices (inclusive).
    let tl = ResidencyTimeline::from_entries(vec![res(18_000, "DC")]).expect("valid");
    assert_eq!(check_import_founding_residency(&tl, Date(18_000)), Ok(()));
    // And one strictly before also suffices.
    assert_eq!(check_import_founding_residency(&tl, Date(18_500)), Ok(()));
}
