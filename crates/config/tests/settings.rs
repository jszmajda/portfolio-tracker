//! RED-phase TDD tests for the CONFIG-SETTINGS-* specs of `config` (see
//! `docs/intent/config/config-specs.md` → "Settings & Read Contract").
//!
//! These exercise the local-file machine/secret settings (never in the
//! workbook, default `US/Eastern` timezone), the workbook-tab domain
//! persistence, the cold-start read (no config tabs → `NoBracketsAvailable`),
//! the hard error on missing/invalid credentials (never empty config), and the
//! workbook-wins cache rebuild on divergence. The scaffold stubs the store
//! bodies with `unimplemented!()`, so each test PANICS (RED).
//!
//! Cents literals are grouped as dollars + trailing cents (e.g. `999_99` reads
//! "$999.99"); clippy's grouping lint is advisory here.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::collections::BTreeMap;

use common::*;
use config::{
    resolve_brackets, BracketState, ConfigData, ConfigError, ConfigStore, InMemoryConfig,
    Jurisdiction, PlatformList, Settings, TaxYear,
};
use pt_core::{Cents, Date};

// @spec CONFIG-SETTINGS-001
#[test]
fn settings_live_in_a_local_file_with_eastern_timezone_default() {
    // The default reporting timezone is US/Eastern (DC and NJ are both Eastern).
    assert_eq!(Settings::default().reporting_timezone, "US/Eastern");

    let settings = Settings {
        workbook_id: "wb-123".to_string(),
        credentials_path: "/secrets/sa.json".to_string(),
        cache_path: "/cache/pt".to_string(),
        reporting_timezone: "US/Eastern".to_string(),
    };
    let store = InMemoryConfig::new(ConfigData::default(), settings.clone());
    let loaded = store.load_settings().expect("settings read from the local file");
    assert_eq!(loaded.workbook_id, "wb-123");
    assert_eq!(loaded.credentials_path, "/secrets/sa.json");
    assert_eq!(loaded.reporting_timezone, "US/Eastern");
}

// @spec CONFIG-SETTINGS-001
#[test]
fn settings_are_not_part_of_domain_config_data() {
    // The domain ConfigData (the workbook half) carries no secret fields — they
    // live in the separate local-file Settings, read via load_settings().
    let store = InMemoryConfig::new(ConfigData::default(), Settings::default());
    let data = store.load().expect("domain load");
    // ConfigData holds only domain config; this compiles because there is no
    // credentials field on it. A cold-start domain load is empty.
    assert!(data.rules_by_year.is_empty());
}

// @spec CONFIG-SETTINGS-002
#[test]
fn domain_config_persists_and_mirrors_for_read_back() {
    let mut store = InMemoryConfig::cold_start();
    let lock = pt_core::NoopLock::new();
    store.put_tax_rules(valid_rules(2025, 19_700), &lock).expect("write");
    store.put_de_minimis(de_minimis_dollar(), &lock).expect("write");
    store.put_platforms(platforms(), &lock).expect("write");

    // A subsequent load reflects the persisted/mirrored domain config.
    let data = store.load().expect("load");
    assert!(data.rules_by_year.contains_key(&TaxYear(2025)));
    assert_eq!(data.de_minimis, de_minimis_dollar());
    assert!(data.platforms.contains("schwab"));
}

// @spec CONFIG-SETTINGS-002
#[test]
fn residency_timeline_persists_and_round_trips_through_the_store() {
    // The residency timeline write path (put_residency) persists to the workbook
    // and mirrors to the cache; a subsequent load round-trips it. (The other
    // domain tables — de-minimis, platforms, aliases, tax-rules — have their own
    // round-trip tests; residency's was the gap.)
    let mut store = InMemoryConfig::cold_start();
    let timeline = config::ResidencyTimeline::from_entries(vec![
        res(18_000, "DC"),
        res(19_000, "NJ"),
    ])
    .expect("a valid founding-and-move timeline");
    store.put_residency(timeline.clone(), &pt_core::NoopLock::new()).expect("residency write");

    let data = store.load().expect("load");
    assert_eq!(data.residency, timeline);
    assert_eq!(data.residency.entries().len(), 2);
    // The stored timeline still resolves residency (the move is inclusive).
    assert_eq!(data.residency.residency_on(Date(19_000)), Some("NJ".to_string()));
}

// @spec CONFIG-SETTINGS-003
#[test]
fn fresh_workbook_with_no_config_tabs_reads_as_cold_start() {
    // A cold-start store models a fresh workbook (no config tabs): empty domain
    // config, so bracket resolution yields NoBracketsAvailable (the seed prompt).
    let store = InMemoryConfig::cold_start();
    let data = store.load().expect("cold-start load is empty, not an error");
    assert!(data.rules_by_year.is_empty());
    let r = resolve_brackets(&data.rules_by_year, &Jurisdiction::Federal, TaxYear(2025));
    assert_eq!(r.state, BracketState::NoBracketsAvailable);
}

// @spec CONFIG-SETTINGS-004
#[test]
fn missing_or_invalid_credentials_is_a_hard_error_not_empty_config() {
    // A store whose secrets file is missing/invalid surfaces a hard error from
    // load_settings — it does NOT silently return empty/default config.
    let store = InMemoryConfig::with_bad_credentials(ConfigData::default());
    assert_eq!(
        store.load_settings(),
        Err(ConfigError::CredentialsUnavailable),
    );
}

// @spec CONFIG-SETTINGS-005
#[test]
fn workbook_wins_over_diverging_cache_on_load() {
    // The authoritative workbook tabs hold 2024 + 2025 rule tables.
    let mut wb_years = BTreeMap::new();
    wb_years.insert(TaxYear(2024), valid_rules(2024, 19_300));
    wb_years.insert(TaxYear(2025), valid_rules(2025, 19_700));
    let workbook = ConfigData {
        rules_by_year: wb_years,
        de_minimis: de_minimis_dollar(),
        residency: config::ResidencyTimeline::new(),
        platforms: platforms(),
        aliases: aliases(),
        display_names: config::DisplayNameMap::default(),
    };

    // The local cache has DIVERGED from the workbook: it is missing the 2025
    // table and carries a stale de-minimis. A truth-bearing cache would surface
    // this; config's must not.
    let mut cache_years = BTreeMap::new();
    cache_years.insert(TaxYear(2024), valid_rules(2024, 19_300));
    let diverging_cache = ConfigData {
        rules_by_year: cache_years,
        de_minimis: config::DeMinimis(Cents(999_99)),
        residency: config::ResidencyTimeline::new(),
        platforms: PlatformList::new(vec!["stale-only".to_string()]),
        aliases: config::AliasMap::default(),
        display_names: config::DisplayNameMap::default(),
    };
    assert_ne!(workbook, diverging_cache, "the scenario must actually diverge");

    let store = InMemoryConfig::with_diverging_cache(workbook.clone(), diverging_cache);

    // The load resolves the divergence: the workbook tabs are authoritative...
    let loaded = store.load().expect("load");
    assert_eq!(loaded, workbook);
    // ...and the cache is REBUILT from the workbook (overwritten, never trusted),
    // not the other way around.
    assert_eq!(store.cache_snapshot(), workbook);
}
