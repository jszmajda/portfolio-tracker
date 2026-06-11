//! RED-phase TDD tests for the trust-boundary drift guard (STORE-GUARD-001/002/003)
//! and the schema (STORE-SCHEMA-001/002/003). The drift guard is the project's
//! `ENGINE-VERIF-014` analog: a wildcard-free exhaustive match over every `Kind`
//! plus a round-trip identity `event → row → event` over every kind and field.
//!
//! Bodies are stubbed `unimplemented!()`, so each test PANICS (RED) until the
//! conversions are implemented.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use store::serde_rows::{
    ledger_to_row, row_to_ledger, row_to_tax, tax_to_row, LEDGER_HEADER, TAX_HEADER,
};
use store::{Row, Tab};

// ---------------------------------------------------------------------------
// STORE-GUARD-002: the round-trip identity over EVERY kind and field. This is
// the drift guard — a kind/field whose mapping is missing fails this test.
// ---------------------------------------------------------------------------

// @spec STORE-GUARD-002
#[test]
fn ledger_event_round_trips_through_row_for_every_kind() {
    for ev in every_ledger_kind() {
        let row = ledger_to_row(&ev);
        let back = row_to_ledger(&row).expect("a well-formed row deserializes");
        assert_eq!(back, ev, "event -> row -> event must be the identity");
    }
}

// @spec STORE-GUARD-002, STORE-SCHEMA-005
#[test]
fn tax_event_round_trips_through_row_for_every_kind() {
    for (ev, id) in every_tax_kind().into_iter().zip(every_tax_id()) {
        let row = tax_to_row(&ev, &id);
        let (back, back_id) = row_to_tax(&row).expect("a well-formed row deserializes");
        assert_eq!(back, ev, "event -> row -> event must be the identity");
        assert_eq!(back_id, id, "the store-assigned EventId must round-trip too");
    }
}

// @spec STORE-GUARD-002
#[test]
fn ledger_round_trip_preserves_seq_id_and_date() {
    // The universal columns (Seq, EventId, Date) are part of the identity, not
    // just the kind payload.
    let ev = sell(42, "evt-42");
    let row = ledger_to_row(&ev);
    let back = row_to_ledger(&row).unwrap();
    assert_eq!(back.seq, ev.seq);
    assert_eq!(back.id, ev.id);
    assert_eq!(back.date, ev.date);
}

// @spec STORE-GUARD-002, STORE-GUARD-004
#[test]
fn list_encodings_round_trip_delimiter_bearing_field_values() {
    use config::{Jurisdiction, TaxYear};
    use ledger_core::{LedgerEvent, LedgerEventKind, LotRef};
    use pt_core::{Cents, Date, MicroShares, Seq};
    use store::serde_rows::{row_to_ledger, row_to_tax};
    use tax::{AccrualKey, Quarter, TaxEvent, TaxEventKind};

    // A Sell whose lot_refs carry ids containing the reserved ':' ';' '~' '%'.
    let sell_nasty = LedgerEvent {
        id: "evt-x".to_string(),
        seq: Seq(1),
        date: Date(19_100),
        kind: LedgerEventKind::Sell {
            sale_id: "sale~1;weird:%".to_string(),
            symbol: "AMZN".to_string(),
            qty: MicroShares(2_000_000),
            unit_price_cents: Cents(175_00),
            fees_cents: Cents(2_00),
            lot_refs: vec![
                LotRef { lot_id: "lot:weird;1".to_string(), qty: MicroShares(1_000_000) },
                LotRef { lot_id: "lot~b%2".to_string(), qty: MicroShares(1_000_000) },
            ],
            accrues_to_state: Some("N;J:".to_string()),
            platform: "schwab".to_string(),
            tracking_code: Some("TC~9".to_string()),
        },
    };
    let row = ledger_to_row(&sell_nasty);
    assert_eq!(
        row_to_ledger(&row).expect("delimiter-bearing Sell round-trips"),
        sell_nasty,
        "lot_id/sale_id with reserved delimiters survive the round-trip"
    );

    // A Pay whose covers carry reserved delimiters in sale_id/lot_id and a state
    // code containing the 'State:' prefix's ':' and the record/field delimiters.
    let pay_nasty = TaxEvent {
        seq: Seq(2),
        kind: TaxEventKind::Pay {
            jurisdiction: Jurisdiction::State("N;J~X:%".to_string()),
            tax_year: TaxYear(2025),
            period: Quarter::Q2,
            amount_cents: Cents(5_000_00),
            date: Date(19_320),
            covers: vec![
                AccrualKey {
                    sale_id: "sale~1;weird".to_string(),
                    lot_id: "lot:weird;1".to_string(),
                    jurisdiction: Jurisdiction::State("D~C".to_string()),
                    tax_year: TaxYear(2025),
                },
                AccrualKey {
                    sale_id: "s%2".to_string(),
                    lot_id: "l;2".to_string(),
                    jurisdiction: Jurisdiction::Federal,
                    tax_year: TaxYear(2024),
                },
            ],
        },
    };
    let row = tax_to_row(&pay_nasty, &"tx-nasty".to_string());
    let (back, back_id) = row_to_tax(&row).expect("delimiter-bearing Pay round-trips");
    assert_eq!(back, pay_nasty, "covers with reserved delimiters round-trip");
    assert_eq!(back_id, "tx-nasty");
}

// @spec STORE-GUARD-002
#[test]
fn sell_fifo_empty_lot_refs_and_none_state_round_trip() {
    // The empty-Vec lot_refs (FIFO fallback) and the None accrues_to_state must
    // survive the round-trip distinctly from a populated case.
    let ev = sell_fifo(7, "fifo-7");
    let row = ledger_to_row(&ev);
    let back = row_to_ledger(&row).unwrap();
    assert_eq!(back, ev);
}

// ---------------------------------------------------------------------------
// STORE-GUARD-001: total, wildcard-free structural match, no arithmetic. We
// assert the conversion is total over every variant (a panic-free row for each)
// and that NO computed/derived numeric field appears — the row carries only the
// stored typed fields, never a basis/gain/tax. (The wildcard-free property is a
// compile-time guarantee in the impl; here we exercise totality.)
// ---------------------------------------------------------------------------

// @spec STORE-GUARD-001
#[test]
fn every_ledger_kind_serializes_without_arithmetic_columns() {
    // The ledger header carries only stored fields; no basis/gain/realized
    // column exists (those are kernel-computed, never stored — STORE-GUARD-001).
    for col in LEDGER_HEADER {
        let c = col.to_lowercase();
        assert!(
            !c.contains("basis") && !c.contains("gain") && !c.contains("realized"),
            "ledger schema must carry no computed accounting column, found {col}"
        );
    }
    // And the conversion is total over every kind.
    for ev in every_ledger_kind() {
        let _ = ledger_to_row(&ev);
    }
}

// @spec STORE-GUARD-001
#[test]
fn every_tax_kind_serializes_without_arithmetic_columns() {
    for col in TAX_HEADER {
        let c = col.to_lowercase();
        assert!(
            !c.contains("basis") && !c.contains("gain") && !c.contains("derived"),
            "tax schema must carry no computed tax column, found {col}"
        );
    }
    for (ev, id) in every_tax_kind().into_iter().zip(every_tax_id()) {
        let _ = tax_to_row(&ev, &id);
    }
}

// ---------------------------------------------------------------------------
// STORE-SCHEMA-001: typed columns including Seq, EventId, Date, Kind, plus the
// union of each family's fields. A JSON-blob column is rejected.
// ---------------------------------------------------------------------------

// @spec STORE-SCHEMA-001
#[test]
fn both_tabs_carry_the_universal_typed_columns() {
    for header in [LEDGER_HEADER, TAX_HEADER] {
        for required in ["Seq", "EventId", "Date", "Kind"] {
            assert!(
                header.contains(&required),
                "every tab must carry the universal column {required}"
            );
        }
    }
}

// @spec STORE-SCHEMA-001
#[test]
fn schema_has_no_json_blob_column() {
    for header in [LEDGER_HEADER, TAX_HEADER] {
        for col in header {
            let c = col.to_lowercase();
            assert!(
                !c.contains("json") && !c.contains("blob") && !c.contains("payload"),
                "a JSON/blob column is rejected (unreadable/unfilterable), found {col}"
            );
        }
    }
}

// @spec STORE-SCHEMA-001
#[test]
fn ledger_kind_populates_only_its_own_fields_sparsely() {
    // A Split row populates Symbol/RatioNum/RatioDen but NOT the Buy/Sell money
    // columns (sparse population — only the columns a Kind uses).
    let row = ledger_to_row(&split(5, "s5"));
    assert_eq!(row.get("Kind"), "Split");
    assert_ne!(row.get("Symbol"), "");
    assert_ne!(row.get("RatioNum"), "");
    assert_ne!(row.get("RatioDen"), "");
    assert_eq!(row.get("UnitPriceCents"), "", "a Split has no price column");
    assert_eq!(row.get("FeesCents"), "", "a Split has no fees column");
    assert_eq!(row.get("LotId"), "", "a Split has no lot column");
}

// @spec STORE-SCHEMA-001
#[test]
fn row_projects_onto_header_order_and_back() {
    let ev = buy(1, "b1");
    let row = ledger_to_row(&ev);
    let cells = row.to_cells(Tab::Ledger);
    assert_eq!(cells.len(), LEDGER_HEADER.len());
    let rebuilt = Row::from_cells(Tab::Ledger, &cells);
    assert_eq!(rebuilt, row, "header projection round-trips the row");
}

// ---------------------------------------------------------------------------
// STORE-LOAD-003 boundary: an unknown Kind or a missing required field is an
// integrity error from the deserializer (never a silent skip). Tested here at
// the conversion level (the load path is exercised in load.rs).
// ---------------------------------------------------------------------------

// @spec STORE-GUARD-001
#[test]
fn unknown_kind_row_is_rejected_not_silently_skipped() {
    let mut row = Row::new();
    row.set("Seq", "1");
    row.set("EventId", "x");
    row.set("Date", "19000");
    row.set("Kind", "Teleport"); // not a real LedgerEventKind
    assert!(row_to_ledger(&row).is_err());
}

// @spec STORE-GUARD-001
#[test]
fn missing_required_field_row_is_rejected() {
    // A Buy row missing its qty column must fail, not default-to-zero silently.
    let mut row = ledger_to_row(&buy(1, "b1"));
    row.cells.remove("Qty");
    assert!(row_to_ledger(&row).is_err());
}

// ---------------------------------------------------------------------------
// STORE-GUARD-003: the serde conversion stays OUTSIDE the verus!{} boundary —
// `store` is the single unverified seam. This is an architectural invariant:
// the crate has no verus!{} module, no vstd dependency, and no verify=true
// metadata marker (unlike ledger-core / tax). We assert it over the crate's own
// manifest + sources so a future drift (someone adding `verus!{}` to the seam)
// fails the test loudly.
// ---------------------------------------------------------------------------

// @spec STORE-GUARD-003
#[test]
fn the_serde_seam_lives_outside_the_verus_boundary() {
    let manifest = include_str!("../Cargo.toml");
    assert!(
        !manifest.contains("vstd"),
        "store must NOT depend on vstd — the serde seam is unverified (STORE-GUARD-003)"
    );
    assert!(
        !manifest.contains("metadata.verus"),
        "store must NOT carry the verify=true marker — it is the unverified trust seam"
    );

    // No source file opens a `verus! {` macro block around the conversion. (The
    // kernels open theirs as `verus! {`; prose mentioning `verus!{}` does not
    // match this block-opening form, so the check is robust to doc comments.)
    for src in [
        include_str!("../src/lib.rs"),
        include_str!("../src/serde_rows.rs"),
        include_str!("../src/cache.rs"),
        include_str!("../src/sheets.rs"),
        include_str!("../src/testkit.rs"),
    ] {
        assert!(
            !src.contains("verus! {") && !src.contains("verus!{\n"),
            "the row<->event serde must stay outside a verus!{{}} block (STORE-GUARD-003)"
        );
    }
}

// @spec STORE-GUARD-002
#[test]
fn absent_and_empty_cells_are_one_identity_across_the_cell_projection() {
    // A spreadsheet grid cannot distinguish an empty cell from an absent one, so
    // a Row that SETS an empty value (the combined migration accrual key's blank
    // sale/lot ids) must equal its own cells→sheet→cells round-trip — otherwise a
    // CORRECT row fails write-verify against the real API (the in-memory fake
    // stores Rows directly and never crosses this projection).
    use config::{Jurisdiction, TaxYear};
    use pt_core::{Cents, Seq};
    use store::serde_rows::tax_to_row;
    use store::{Row, Tab};
    use tax::{AccrualKey, TaxEvent, TaxEventKind};

    let ev = TaxEvent {
        seq: Seq(2),
        kind: TaxEventKind::AmountOverride {
            accrual_key: AccrualKey {
                sale_id: String::new(), // the combined-key blanks
                lot_id: String::new(),
                jurisdiction: Jurisdiction::Federal,
                tax_year: TaxYear(2021),
            },
            applied_amount_cents: Cents(25_363),
            reason: "migration: legacy actual tax".to_string(),
        },
    };
    let written = tax_to_row(&ev, &"tax-x".to_string());
    let round_tripped = Row::from_cells(Tab::Tax, &written.to_cells(Tab::Tax));
    assert_eq!(
        written, round_tripped,
        "a Row must equal its own cell-projection round-trip (empty ⇔ absent)"
    );
}
