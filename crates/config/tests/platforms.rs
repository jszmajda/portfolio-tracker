//! RED-phase TDD tests for the CONFIG-PLATFORM-* specs of `config` (see
//! `docs/intent/config/config-specs.md` → "Platforms").
//!
//! These exercise the non-constraining platform suggestion list (an event may
//! name a platform absent from the list) and the symbol→ticker alias map with an
//! IDENTITY default. The scaffold stubs `AliasMap::resolve` with
//! `unimplemented!()`, so that test PANICS (RED).

mod common;

use std::collections::BTreeMap;

use config::{AliasMap, ConfigStore, InMemoryConfig, PlatformList};

// @spec CONFIG-PLATFORM-001
#[test]
fn platform_list_suggests_without_constraining_entry() {
    let mut store = InMemoryConfig::cold_start();
    let list = PlatformList::new(vec!["schwab".to_string(), "fidelity".to_string()]);
    store
        .put_platforms(list, &pt_core::NoopLock::new())
        .expect("platforms write");

    let data = store.load().expect("load");
    // The list offers suggestions...
    assert!(data.platforms.contains("schwab"));
    // ...but does not constrain: a platform absent from the list is not in it,
    // yet the list carries no notion of rejecting it (it is advisory only). The
    // type is membership-only — there is no reject/validate API to misuse.
    assert!(!data.platforms.contains("robinhood"));
    assert_eq!(data.platforms.names().len(), 2);
    // NOTE (CONFIG-PLATFORM-001 coverage boundary): the GENUINE non-constraint
    // property — that an event naming an absent platform is still ACCEPTED on
    // entry/import — belongs to the event-entry consumer, which does not exist in
    // this crate. It must be asserted at that call site as a cross-segment test
    // (import/tui). This in-segment test only proves the list type is advisory
    // (membership-only, no reject path); do not mistake its green for full
    // coverage of the spec's downstream non-constraint guarantee.
}

// @spec CONFIG-PLATFORM-002
#[test]
fn alias_map_resolves_with_identity_default() {
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    m.insert("BRK".to_string(), "BRK.B".to_string());
    let aliases = AliasMap::new(m);

    // A symbol WITH an alias resolves to the exchange/class-share form.
    assert_eq!(aliases.resolve("BRK"), "BRK.B".to_string());
    // A symbol WITHOUT an alias resolves to itself (identity default).
    assert_eq!(aliases.resolve("AMZN"), "AMZN".to_string());
}

// @spec CONFIG-PLATFORM-002
#[test]
fn alias_map_round_trips_through_store() {
    let mut store = InMemoryConfig::cold_start();
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    m.insert("GOOG".to_string(), "NASDAQ:GOOG".to_string());
    store
        .put_aliases(AliasMap::new(m), &pt_core::NoopLock::new())
        .expect("aliases write");

    let data = store.load().expect("load");
    assert_eq!(data.aliases.resolve("GOOG"), "NASDAQ:GOOG".to_string());
    assert_eq!(data.aliases.resolve("TSLA"), "TSLA".to_string());
}

// @spec CONFIG-PLATFORM-003
#[test]
fn display_name_map_resolves_with_ticker_fallback() {
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    m.insert("AMZN".to_string(), "Amazon.com".to_string());
    let names = config::DisplayNameMap::new(m);

    // A symbol WITH a display name resolves to the company name.
    assert_eq!(names.resolve("AMZN"), "Amazon.com".to_string());
    // An unmapped symbol resolves to the ticker itself (the honest degrade).
    assert_eq!(names.resolve("PLTR"), "PLTR".to_string());
}

// @spec CONFIG-PLATFORM-003
#[test]
fn display_name_map_round_trips_through_store() {
    let mut store = InMemoryConfig::cold_start();
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    m.insert("GOOGL".to_string(), "Alphabet".to_string());
    store
        .put_display_names(config::DisplayNameMap::new(m), &pt_core::NoopLock::new())
        .expect("display names write");

    let data = store.load().expect("load");
    assert_eq!(data.display_names.resolve("GOOGL"), "Alphabet".to_string());
    assert_eq!(data.display_names.resolve("CSCO"), "CSCO".to_string());
}
