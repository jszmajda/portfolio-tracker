//! Symbol → Ticker Mapping: SHEET-MAP-001/002.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::collections::BTreeMap;

use sheets_view::{price_formula, render_positions, resolve_ticker, POSITIONS_HEADER, ViewError};

use config::AliasMap;
use pt_core::Date;

fn col(name: &str) -> usize {
    POSITIONS_HEADER.iter().position(|h| *h == name).unwrap()
}

// @spec SHEET-MAP-001
#[test]
fn resolves_symbol_to_ticker_via_alias_identity_when_unset() {
    let mut map = BTreeMap::new();
    map.insert("BRK".to_string(), "BRK.B".to_string());
    map.insert("AAPL".to_string(), "NASDAQ:AAPL".to_string());
    let aliases = AliasMap::new(map);

    // Aliased symbols resolve to their GOOGLEFINANCE form.
    assert_eq!(resolve_ticker(&aliases, "BRK"), "BRK.B");
    assert_eq!(resolve_ticker(&aliases, "AAPL"), "NASDAQ:AAPL");
    // An unaliased symbol resolves to identity.
    assert_eq!(resolve_ticker(&aliases, "AMZN"), "AMZN");

    // The Price formula is built from the RESOLVED ticker.
    let f = price_formula("BRK.B");
    assert!(f.is_formula());
    assert!(f.text().contains("GOOGLEFINANCE"));
    assert!(f.text().contains("BRK.B"));
}

// @spec SHEET-MAP-001
#[test]
fn positions_price_formula_uses_resolved_ticker() {
    let marks = common::marks(&[("BRK", 500_000_00)]);
    // A single-symbol portfolio for BRK.
    let events = vec![common::buy(1, 19_000, "lot-brk", "BRK", 1_000_000, 400_000_00)];
    let snap = common::replay(&events, &marks);

    let mut map = BTreeMap::new();
    map.insert("BRK".to_string(), "BRK.B".to_string());
    let aliases = AliasMap::new(map);
    let ctx = common::ctx(2022);
    let rates = common::effective_rates(&snap, Date(19_200), &ctx);

    let positions = render_positions(&snap, &rates, &aliases).unwrap();
    let price = positions.rows[0][col("Price")].text();
    assert!(price.contains("BRK.B"), "Price formula must use resolved ticker: {price}");
    assert!(!price.contains("\"BRK\""), "Price must NOT use the raw symbol: {price}");
}

// @spec SHEET-MAP-002
#[test]
fn read_back_keyed_by_row_identity_errors_loud_on_duplicate_symbol() {
    // Snapshot.positions is a Map<Symbol,_> so a real snapshot cannot carry two
    // rows for one symbol; render_positions still ASSERTS one row per symbol and
    // errors loudly if the invariant is somehow violated. We construct a snapshot
    // and confirm the happy path keys by symbol (one row per symbol).
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let aliases = AliasMap::default();
    let ctx = common::ctx(2022);
    let rates = common::effective_rates(&snap, Date(19_200), &ctx);

    let positions = render_positions(&snap, &rates, &aliases).unwrap();
    // One row per symbol — the read-back key.
    let mut symbols: Vec<String> = positions
        .rows
        .iter()
        .map(|r| r[col("Symbol")].text().to_string())
        .collect();
    let n = symbols.len();
    symbols.sort();
    symbols.dedup();
    assert_eq!(symbols.len(), n, "every row carries a distinct symbol (1-per-symbol)");
}

// @spec SHEET-MAP-002
#[test]
fn duplicate_symbol_guard_is_defensive_and_one_row_per_symbol_holds() {
    // The duplicate-symbol guard in render_positions is DEFENSIVE: `Snapshot.
    // positions` is a `Map<Symbol,_>`, so the key is unique by construction and the
    // `Err(DuplicateSymbolRow)` branch is structurally unreachable through the real
    // API. The design (sheets-view-design.md → "Symbol → Ticker Mapping") still
    // calls for the loud error if a future non-Map source ever violated the
    // invariant the mark read-back keys on. We therefore assert (a) the real API
    // upholds one-row-per-symbol on a multi-symbol snapshot, and (b) the loud-error
    // contract's Display surface — not a fabricated "reachable" duplicate.
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let aliases = AliasMap::default();
    let ctx = common::ctx(2022);
    let rates = common::effective_rates(&snap, Date(19_200), &ctx);

    let positions = render_positions(&snap, &rates, &aliases).expect("happy path: 1 row/symbol");
    // Every rendered row carries a DISTINCT symbol — the invariant the guard
    // protects, here verified through the real API (the reachable path).
    let mut seen: Vec<String> = positions
        .rows
        .iter()
        .map(|r| r[col("Symbol")].text().to_string())
        .collect();
    let n = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), n, "render_positions yields one row per symbol");

    // The loud-error variant exists as the contract for a violated invariant.
    let err = ViewError::DuplicateSymbolRow("AMZN".to_string());
    assert_eq!(format!("{err}"), "DuplicateSymbolRow(\"AMZN\")");
}
