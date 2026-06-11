//! RED-phase TDD tests for the CONFIG-TAXRULES-* specs of `config` (see
//! `docs/intent/config/config-specs.md` → "Tax-Rule Tables").
//!
//! These exercise the per-`tax_year` rule-table storage, the `ppm`/`Cents`
//! units, the live open-year income, the `last_verified`/`source_note`
//! provenance, the one-status-per-year rule, and the single de-minimis
//! threshold. They construct the real public types and round-trip them through
//! the `ConfigStore` fake. The scaffold stubs the store `put_*`/`load` bodies
//! with `unimplemented!()`, so each test PANICS (RED) until implemented.
//!
//! Cents literals are grouped as dollars + trailing cents (e.g. `250_000_00`
//! reads "$250,000.00"); clippy's grouping lint is advisory here.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::collections::BTreeMap;

use common::*;
use config::{
    BracketState, ConfigData, ConfigStore, DeMinimis, FilingStatus, InMemoryConfig, Jurisdiction,
    Settings, TaxYear,
};
use pt_core::{Cents, Date};

// @spec CONFIG-TAXRULES-001
#[test]
fn stores_per_year_filing_status_brackets_niit_and_state_sets() {
    let mut store = InMemoryConfig::cold_start();
    store
        .put_tax_rules(valid_rules(2025, 19_700), &pt_core::NoopLock::new())
        .expect("a valid rule table is accepted");

    let data = store.load().expect("load after write");
    let r = data
        .rules_by_year
        .get(&TaxYear(2025))
        .expect("the 2025 rule table is stored");

    // Filing status, federal ordinary, federal long-term, NIIT, and per-state
    // ordinary brackets are all present and keyed by tax_year.
    assert_eq!(r.filing_status, FilingStatus::MarriedFilingJointly);
    assert!(!r.federal_ordinary.rows.is_empty());
    assert!(!r.federal_long_term.rows.is_empty());
    assert_eq!(r.niit.rate_ppm, config::Ppm(38_000));
    assert!(r.state_ordinary.contains_key("DC"));
    assert!(r.state_ordinary.contains_key("NJ"));
}

// @spec CONFIG-TAXRULES-002
#[test]
fn rates_are_ppm_and_thresholds_are_cents_exact() {
    // NJ's 5.525% is representable exactly as 55_250 ppm (bps cannot: 552.5).
    let r = valid_rules(2025, 19_700);
    let nj = r.state_ordinary.get("NJ").expect("NJ set");
    assert_eq!(nj.top_rate(), config::Ppm(55_250));
    // Thresholds are Cents (i64), not dollars/floats.
    assert_eq!(nj.rows[0].lower_threshold_cents, Cents(0));
    assert_eq!(r.niit.magi_threshold_cents, Cents(250_000_00));
}

// @spec CONFIG-TAXRULES-003
#[test]
fn open_year_income_is_live_and_updatable() {
    let mut store = InMemoryConfig::cold_start();
    store
        .put_tax_rules(valid_rules(2026, 19_900), &pt_core::NoopLock::new())
        .expect("initial open-year table");

    // Update the open year's income estimate; the latest write wins.
    let mut updated = valid_rules(2026, 19_900);
    updated.ordinary_income_cents = Cents(450_000_00);
    store
        .put_tax_rules(updated, &pt_core::NoopLock::new())
        .expect("re-writing the open year updates its income");

    let data = store.load().expect("load");
    assert_eq!(
        data.rules_by_year
            .get(&TaxYear(2026))
            .unwrap()
            .ordinary_income_cents,
        Cents(450_000_00),
    );
}

// @spec CONFIG-TAXRULES-004
#[test]
fn each_rule_set_records_last_verified_and_source_note() {
    let r = valid_rules(2025, 19_700);
    assert_eq!(r.federal_ordinary.last_verified, Date(19_700));
    assert_eq!(r.federal_ordinary.source_note, "test source");
    assert_eq!(r.federal_long_term.last_verified, Date(19_700));
    assert_eq!(r.state_ordinary.get("DC").unwrap().last_verified, Date(19_700));
}

// @spec CONFIG-TAXRULES-005
#[test]
fn records_one_filing_status_per_year() {
    let mut store = InMemoryConfig::cold_start();
    let mut y2024 = valid_rules(2024, 19_300);
    y2024.filing_status = FilingStatus::Single;
    let y2025 = valid_rules(2025, 19_700); // MFJ from the helper default
    let lock = pt_core::NoopLock::new();
    store.put_tax_rules(y2024, &lock).expect("2024");
    store.put_tax_rules(y2025, &lock).expect("2025");

    let data = store.load().expect("load");
    assert_eq!(
        data.rules_by_year.get(&TaxYear(2024)).unwrap().filing_status,
        FilingStatus::Single,
    );
    assert_eq!(
        data.rules_by_year.get(&TaxYear(2025)).unwrap().filing_status,
        FilingStatus::MarriedFilingJointly,
    );
}

// @spec CONFIG-TAXRULES-006
#[test]
fn stores_single_de_minimis_threshold() {
    let mut store = InMemoryConfig::cold_start();
    store
        .put_de_minimis(DeMinimis(Cents(100)), &pt_core::NoopLock::new())
        .expect("$1 de-minimis is accepted");
    let data = store.load().expect("load");
    assert_eq!(data.de_minimis, DeMinimis(Cents(100)));
}

// A sanity check that `load` on a populated, non-cold-start store round-trips
// the de-minimis and rules together (TAXRULES-006 + 001 integration).
// @spec CONFIG-TAXRULES-006
#[test]
fn populated_store_round_trips_full_config_data() {
    let mut rules_by_year = BTreeMap::new();
    rules_by_year.insert(TaxYear(2025), valid_rules(2025, 19_700));
    let data = ConfigData {
        rules_by_year,
        de_minimis: de_minimis_dollar(),
        residency: config::ResidencyTimeline::new(),
        platforms: platforms(),
        aliases: aliases(),
        display_names: config::DisplayNameMap::default(),
    };
    let store = InMemoryConfig::new(data.clone(), Settings::default());
    let loaded = store.load().expect("load populated");
    assert_eq!(loaded.de_minimis, de_minimis_dollar());
    assert_eq!(
        config::resolve_brackets(&loaded.rules_by_year, &Jurisdiction::Federal, TaxYear(2025)).state,
        BracketState::Verified,
    );
}
