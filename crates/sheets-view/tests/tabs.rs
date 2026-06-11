//! View Tabs & Layout: SHEET-TAB-001/002/003.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use sheets_view::testkit::InMemorySheetsView;
use sheets_view::{
    render_open_lots, render_positions, render_realized, render_tax, workbook_tab_order, Cell,
    Publisher, SheetsViewClient, ViewTab, HISTORY_TAB, LEDGER_EVENTS_TAB, OPEN_LOTS_TAB,
    PLATFORMS_ALIASES_TAB, POSITIONS_TAB, REALIZED_TAB, RESIDENCY_TAB, TAX_EVENTS_TAB,
    TAX_RESERVE_HEADER, TAX_RULES_TAB, TAX_TAB, VIEW_TABS,
};

use config::AliasMap;
use pt_core::{Date, NoopLock};

// @spec SHEET-TAB-001
#[test]
fn publishes_four_view_tabs_one_row_per_entity() {
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let ctx = common::ctx(2022);
    let as_of = Date(19_200);
    let rates = common::effective_rates(&snap, as_of, &ctx);
    let aliases = AliasMap::default();
    let accruals = common::accruals(&snap, &ctx);
    let annual = common::annual_rows(&snap, &[], &ctx);

    let positions = render_positions(&snap, &rates, &aliases).expect("positions");
    let open_lots = render_open_lots(&snap, as_of);
    let realized = render_realized(&snap, &accruals);
    let tax = render_tax(&accruals, &annual);

    // Four tabs, the expected names.
    assert_eq!(positions.name, POSITIONS_TAB);
    assert_eq!(open_lots.name, OPEN_LOTS_TAB);
    assert_eq!(realized.name, REALIZED_TAB);
    assert_eq!(tax.accruals.name, TAX_TAB);
    assert_eq!(VIEW_TABS.len(), 4);

    // Typed headers present (non-empty), no among-data summary rows in Positions:
    // exactly one row per position symbol (AMZN + GOOG = 2).
    assert!(!positions.header.is_empty());
    assert_eq!(positions.rows.len(), snap.positions.len());
    assert_eq!(positions.rows.len(), 2);

    // Open Lots: one row per open lot.
    assert_eq!(open_lots.rows.len(), snap.open_lots.len());
    // Realized: one row per realized gain.
    assert_eq!(realized.rows.len(), snap.realized_gains.len());

    // Each row is exactly as wide as its header (typed columns, no ragged rows).
    for row in &positions.rows {
        assert_eq!(row.len(), positions.header.len());
    }
    for row in &open_lots.rows {
        assert_eq!(row.len(), open_lots.header.len());
    }

    // The Tax tab's accrual band is one row per accrual under TAX_HEADER, with NO
    // among-data summary rows (no blank separator, no "RESERVE SUMMARY" label, no
    // embedded second header) — so native Sheets filtering/sorting works directly.
    // The reserve summary lives in its OWN typed band. (SHEET-TAB-001)
    assert_eq!(tax.accruals.rows.len(), accruals.len());
    for row in &tax.accruals.rows {
        assert_eq!(
            row.len(),
            tax.accruals.header.len(),
            "no ragged/embedded rows"
        );
        let first = row[0].text();
        assert!(
            !first.is_empty(),
            "no blank/label/separator row in the accrual band: {row:?}"
        );
        assert_ne!(first, "RESERVE SUMMARY", "no among-data label row");
        // No embedded second header masquerading as data.
        assert_ne!(
            row.iter().map(|c| c.text().to_string()).collect::<Vec<_>>(),
            TAX_RESERVE_HEADER
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            "no embedded reserve header inside the accrual band"
        );
    }
}

// @spec SHEET-TAB-001, SHEET-TAB-004
#[test]
fn tax_accrual_band_has_no_among_data_summary_rows() {
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let ctx = common::ctx(2022);
    let accruals = common::accruals(&snap, &ctx);
    let annual = common::annual_rows(&snap, &[], &ctx);

    let tax = render_tax(&accruals, &annual);

    // The accrual band and the reserve summary are DISTINCT typed ranges, each with
    // its own frozen header — the reserve summary never sits inside the accrual
    // band's filterable range. (SHEET-TAB-001, SHEET-FORMULA-004)
    assert_eq!(tax.accruals.name, TAX_TAB);
    assert_ne!(tax.reserve_summary.name, tax.accruals.name);
    assert_eq!(
        tax.reserve_summary.header,
        TAX_RESERVE_HEADER
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );

    // Every accrual-band row is a genuine accrual (no blank/label/embedded-header
    // row), so the band is natively filterable end to end.
    assert_eq!(tax.accruals.rows.len(), accruals.len());
    for row in &tax.accruals.rows {
        assert_eq!(row.len(), tax.accruals.header.len());
        assert!(!row[0].text().is_empty());
    }
    // The reserve summary band is one row per (jurisdiction, tax_year) under its
    // own header — also no among-data rows.
    assert_eq!(tax.reserve_summary.rows.len(), annual.len());
    for row in &tax.reserve_summary.rows {
        assert_eq!(row.len(), TAX_RESERVE_HEADER.len());
    }
}

// @spec SHEET-TAB-002
#[test]
fn workbook_order_views_then_config_then_logs_positions_landing() {
    let order = workbook_tab_order();
    // Positions is the leftmost (landing) tab.
    assert_eq!(order.first().copied(), Some(POSITIONS_TAB));
    // Views first, in their order.
    assert_eq!(
        &order[0..4],
        &[POSITIONS_TAB, OPEN_LOTS_TAB, REALIZED_TAB, TAX_TAB]
    );
    // History (reports) sits among the views in the ordering.
    let pos_history = order.iter().position(|t| *t == HISTORY_TAB).unwrap();
    // Config tabs after views/History.
    let pos_tax_rules = order.iter().position(|t| *t == TAX_RULES_TAB).unwrap();
    let pos_residency = order.iter().position(|t| *t == RESIDENCY_TAB).unwrap();
    let pos_platforms = order
        .iter()
        .position(|t| *t == PLATFORMS_ALIASES_TAB)
        .unwrap();
    // Event logs are last (far right).
    let pos_ledger = order.iter().position(|t| *t == LEDGER_EVENTS_TAB).unwrap();
    let pos_tax_events = order.iter().position(|t| *t == TAX_EVENTS_TAB).unwrap();

    let pos_tax_view = order.iter().position(|t| *t == TAX_TAB).unwrap();
    assert!(pos_tax_view < pos_history);
    assert!(pos_history < pos_tax_rules);
    assert!(pos_tax_rules < pos_residency && pos_residency < pos_platforms);
    assert!(pos_platforms < pos_ledger);
    assert!(pos_ledger < pos_tax_events);
    // Event logs are the final two tabs.
    assert_eq!(order.last().copied(), Some(TAX_EVENTS_TAB));
}

// @spec SHEET-TAB-002
#[test]
fn republish_idempotently_reasserts_tab_order() {
    let client = InMemorySheetsView::new();
    let mut pub_ = Publisher::new(client);

    let positions = ViewTab {
        name: POSITIONS_TAB.to_string(),
        header: vec!["Symbol".to_string()],
        rows: vec![],
    };
    // Two republishes both leave the tab order equal to workbook_tab_order().
    let _ = pub_.republish(&[positions.clone()], &NoopLock::new(), "2022-09-01");
    let order1 = pub_.client().tab_order();
    let _ = pub_.republish(&[positions], &NoopLock::new(), "2022-09-02");
    let order2 = pub_.client().tab_order();

    let expected: Vec<String> = workbook_tab_order().iter().map(|s| s.to_string()).collect();
    assert_eq!(order1, expected);
    assert_eq!(order2, expected); // idempotent
}

// @spec SHEET-TAB-003
#[test]
fn republish_regenerates_data_and_never_rewrites_frozen_header() {
    let mut client = InMemorySheetsView::new();
    // First publish: header + 2 rows.
    let v1 = ViewTab {
        name: POSITIONS_TAB.to_string(),
        header: vec!["Symbol".to_string(), "Shares".to_string()],
        rows: vec![
            vec![
                Cell::Value("AMZN".to_string()),
                Cell::Value("3".to_string()),
            ],
            vec![
                Cell::Value("GOOG".to_string()),
                Cell::Value("1".to_string()),
            ],
        ],
    };
    client.batch_update_view(&v1).unwrap();
    let p1 = client.published(POSITIONS_TAB).unwrap();
    assert!(p1.header_frozen);
    assert_eq!(p1.rows.len(), 2);

    // Republish with the SAME header but fewer rows (a human-inserted/sorted row
    // is overwritten by design; the data range is regenerated). The frozen header
    // is byte-identical.
    let v2 = ViewTab {
        name: POSITIONS_TAB.to_string(),
        header: vec!["Symbol".to_string(), "Shares".to_string()],
        rows: vec![vec![
            Cell::Value("AMZN".to_string()),
            Cell::Value("3".to_string()),
        ]],
    };
    client.batch_update_view(&v2).unwrap();
    let p2 = client.published(POSITIONS_TAB).unwrap();
    assert_eq!(p2.header, p1.header); // never rewritten
    assert_eq!(p2.rows.len(), 1); // regenerated, tail truncated
}
