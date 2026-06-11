//! The legacy-workbook parse: SYNTHETIC grids reproducing the real tabs' layout
//! and every pathology (the `Stock Actions` note row, `$`/comma money,
//! `#DIV/0!` cells, blank fee cells, and `Positions` furniture rows) parse into
//! the typed legacy rows; required-cell failures route to the malformed list.

use import::parse::{
    parse_legacy_date, parse_legacy_tabs, parse_shares_to_micro, Grid, PlatformAssignment,
};
use import::LegacyActionKind;
use pt_core::{Cents, Date, MicroShares};

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|s| s.to_string()).collect()
}

fn platforms() -> PlatformAssignment {
    let mut per_symbol = std::collections::BTreeMap::new();
    per_symbol.insert("AMZN".to_string(), "vestco".to_string());
    let mut id_prefixes = std::collections::BTreeMap::new();
    id_prefixes.insert("ob-".to_string(), "OtherBroker".to_string());
    PlatformAssignment {
        default_platform: "schwab".to_string(),
        id_prefixes,
        per_symbol,
    }
}

/// The legacy `Stock Actions` shape: a note row ABOVE the header, then the
/// header, then Buy + Vest rows (the Vest carries FMV in `$/share` with $0 cost).
fn actions_grid() -> Grid {
    vec![
        row(&["Orange is auto"]),
        row(&[
            "ID",
            "Date",
            "Stock",
            "Activity",
            "Shares",
            "rm. shs",
            "$/share",
            "Commission",
            "Fees",
            "Total Cost",
        ]),
        row(&[
            "1", "1/1/2015", "CSCO", "Buy", "8", "8", "$25.50", "$0.00", "$0.00", "$204.00",
        ]),
        row(&[
            "RSU-GRANT1-a",
            "11/15/2021",
            "AMZN",
            "Vest",
            "250",
            "0",
            "$180.00",
            "$0.00",
            "$0.00",
            "$0.00",
        ]),
        row(&["", "", "", "", "", "", "", "", "", ""]), // residual blank row
    ]
}

/// The legacy `Stock Sales` shape: header on row 1; a tranche id REUSED across
/// sales; blank Commission/Fees cells; a $0 sell-to-cover row.
fn sales_grid() -> Grid {
    vec![
        row(&[
            "ID",
            "Date",
            "Stock",
            "Shares Sold",
            "$/share",
            "Cost",
            "Commission",
            "Fees",
            "Sales - Fees",
            "Profit",
            "ClcTax Rate",
            "UsedTaxRate",
            "Tax",
            "Net Profit",
            "Notes",
        ]),
        row(&[
            "RSU-GRANT1-a",
            "11/15/2021",
            "AMZN",
            "100",
            "$0.00",
            "$0.00",
            "$0.00",
            "$0.00",
            "$0.00",
            "$0.00",
            "0.00%",
            "0.00%",
            "$0.00",
            "$0.00",
            "sell to cover vest",
        ]),
        row(&[
            "RSU-GRANT1-a",
            "2/7/2023",
            "AMZN",
            "100",
            "$120.00",
            "$0.00",
            "",
            "",
            "$12,000.00",
            "$12,000.00",
            "29.75%",
            "20.00%",
            "$2,400.00",
            "$9,600.00",
        ]),
    ]
}

/// The legacy `Positions` shape: header row 0; `#DIV/0!` in DERIVED columns;
/// blank realized cells on never-sold symbols; money with `$` and commas.
fn positions_grid() -> Grid {
    vec![
        row(&[
            "Stock",
            "Name",
            "Cur Shs",
            "avg $cst/shr",
            "Total Rm Cost",
            "D?",
            "Goal Date",
            "Cur $/s",
            "Current total",
            "",
            "Unr. Profit",
            "Tax",
            "Net Unr. Profit",
            "unr%",
            "",
            "Realized Profit",
        ]),
        row(&[
            "AMZN",
            "Amazon.com, Inc.",
            "750",
            "$0.30",
            "$0.00",
            "",
            "",
            "$240.00",
            "$180,000.00",
            "",
            "$180,000.00",
            "$54,000.00",
            "$126,000.00",
            "#DIV/0!",
            "",
            "$10,500.00",
        ]),
        row(&[
            "O",
            "Realty Income Corporation",
            "12",
            "$60.00",
            "$720.00",
            "Y",
            "",
            "$55.00",
            "$660.00",
            "",
            "-$60.00",
            "$0.00",
            "-$60.00",
            "-8%",
            "",
            "",
        ]),
    ]
}

// @spec IMPORT-RUN-008, IMPORT-RUN-009
#[test]
fn the_real_layout_parses_into_typed_rows() {
    let parsed = parse_legacy_tabs(
        &actions_grid(),
        &sales_grid(),
        &positions_grid(),
        &platforms(),
    );

    assert_eq!(
        parsed.malformed,
        vec![],
        "the real layout has no malformed rows"
    );

    // Actions: the note row + header were skipped; Buy and Vest both landed.
    assert_eq!(parsed.actions.len(), 2);
    let csco = &parsed.actions[0];
    assert_eq!(csco.tranche_id, "1");
    assert_eq!(csco.kind, LegacyActionKind::Buy);
    assert_eq!(csco.qty, MicroShares(8_000_000));
    assert_eq!(csco.dollars_per_share, "25.50");
    assert_eq!(csco.date, Date(16_436)); // 2015-01-01
    assert_eq!(csco.platform, "schwab", "default platform");
    assert_eq!(
        csco.coord.row, 3,
        "1-based sheet row, clickable in the legacy tab"
    );

    let vest = &parsed.actions[1];
    assert_eq!(vest.kind, LegacyActionKind::Vest);
    assert_eq!(
        vest.dollars_per_share, "180.00",
        "the Vest FMV survives the parse"
    );
    assert_eq!(vest.platform, "vestco", "per-symbol platform override");

    // Sales: the reused tranche id got row-unique sale tags; blank fee cells
    // read as zero; the $0 sell-to-cover row is carried (the engine handles it).
    assert_eq!(parsed.sales.len(), 2);
    assert_eq!(parsed.sales[0].tranche_id, "RSU-GRANT1-a");
    assert_eq!(parsed.sales[1].tranche_id, "RSU-GRANT1-a");
    assert_ne!(
        parsed.sales[0].sale_tag, parsed.sales[1].sale_tag,
        "locally-unique tags"
    );
    assert_eq!(
        parsed.sales[1].fees_dollars, "0.00",
        "blank fee cells are zero"
    );
    assert_eq!(parsed.sales[1].dollars_per_share, "120.00");

    // Positions: reconcile targets parsed exactly; the REALIZED target is the
    // sales tab's pre-tax ΣProfit per symbol (the Positions "Realized Profit"
    // cell is post-tax net and is replaced at the boundary): the fixture's two
    // AMZN sales carry Profit $0.00 (STC) + $12,000.00.
    assert_eq!(parsed.positions.len(), 2);
    let amzn = &parsed.positions[0];
    assert_eq!(amzn.shares, MicroShares(750_000_000));
    assert_eq!(amzn.realized_pnl_cents, Cents(1_200_000));
    assert_eq!(amzn.unrealized_cents, Cents(18_000_000));
    assert_eq!(parsed.profit_sums.get("AMZN"), Some(&Cents(1_200_000)));
    assert_eq!(
        parsed.positions[1].realized_pnl_cents,
        Cents(0),
        "no sales = zero target"
    );
    assert_eq!(
        parsed.positions[1].unrealized_cents,
        Cents(-6_000),
        "negative money parses"
    );
    assert_eq!(parsed.marks.get("AMZN"), Some(&Cents(24_000)));
    assert_eq!(parsed.marks.get("O"), Some(&Cents(5_500)));
}

// @spec IMPORT-MAP-005, IMPORT-CORP-006, IMPORT-RUN-010
#[test]
fn exercise_split_and_grant_activities_map_per_the_owner_vocabulary() {
    let mut grid = actions_grid();
    grid.push(row(&[
        "OPT-7",
        "2/23/2023",
        "PLTR",
        "Exercise",
        "15000",
        "0",
        "$12.00",
        "$0.00",
        "$0.00",
        "$135,757.31",
    ]));
    grid.push(row(&[
        "31", "6/6/2022", "AMZN", "Split", "57", "0", "$0.00", "", "", "",
    ]));
    grid.push(row(&[
        "G-900",
        "1/15/2025",
        "WXYZ",
        "Grant",
        "500",
        "500",
        "$0.00",
        "",
        "",
        "",
    ]));
    grid.push(row(&[
        "ob-28",
        "11/20/2025",
        "NVDA",
        "Buy",
        "5",
        "5",
        "$100.00",
        "$0.00",
        "$0.00",
        "$500.00",
    ]));

    let parsed = parse_legacy_tabs(&grid, &sales_grid(), &positions_grid(), &platforms());

    assert!(
        parsed.malformed.is_empty(),
        "none of these are malformed: {:?}",
        parsed.malformed
    );

    // The Exercise is a Buy carrying the provenance note — and the true-cash
    // basis rule re-derives the blended strike from Total Cost (the $/share
    // cell aggregates multiple strikes): ⌊13,575,731¢ / 15,000 sh⌋ = 905¢,
    // remainder 731¢ folded into fees, basis = the cash paid to the cent.
    let pltr = parsed
        .actions
        .iter()
        .find(|a| a.tranche_id == "OPT-7")
        .expect("exercise lands");
    assert_eq!(pltr.kind, LegacyActionKind::Buy);
    assert_eq!(pltr.tracking_code.as_deref(), Some("exercise"));
    assert_eq!(pltr.dollars_per_share, "9.05");
    assert_eq!(pltr.fees_dollars, "7.31");

    // Split + Grant rows are SKIPPED with reasons, never reconstructed rows.
    assert_eq!(parsed.skipped.len(), 2);
    assert!(parsed.skipped[0].reason.contains("Split delta row"));
    assert!(parsed.skipped[1].reason.contains("unvested Grant"));
    assert!(!parsed
        .actions
        .iter()
        .any(|a| a.tranche_id == "31" || a.tranche_id == "G-900"));

    // The id-prefix platform mapping wins over the default.
    let nvda = parsed
        .actions
        .iter()
        .find(|a| a.tranche_id == "ob-28")
        .expect("buy lands");
    assert_eq!(nvda.platform, "OtherBroker");
}

// @spec IMPORT-MAP-006
#[test]
fn a_rounded_dollar_per_share_yields_the_true_cash_basis_from_total_cost() {
    let mut grid = actions_grid();
    // Sub-dollar OTC shape: 72,000 shares, $/share displays $0.07, Total Cost
    // $5,069.00 (a display-rounded per-share cell vs the true cash paid).
    grid.push(row(&[
        "b-34",
        "12/29/2025",
        "MICROCO",
        "Buy",
        "72000",
        "72000",
        "$0.07",
        "$0.00",
        "$0.00",
        "$5,069.00",
    ]));
    let parsed = parse_legacy_tabs(&grid, &sales_grid(), &positions_grid(), &platforms());

    let r = parsed
        .actions
        .iter()
        .find(|a| a.tranche_id == "b-34")
        .expect("row lands");
    assert_eq!(
        r.dollars_per_share, "0.07",
        "unit = floor(506900/72000) = 7 cents"
    );
    assert_eq!(r.fees_dollars, "29.00", "the exact remainder lands in fees");
    // basis = 72,000 × 7¢ + 2,900¢ = 506,900¢ = the legacy cash to the cent.

    // A row whose Total Cost AGREES is untouched (the CSCO buy: 8 × $25.50 = $204.00).
    let csco = parsed.actions.iter().find(|a| a.tranche_id == "1").unwrap();
    assert_eq!(csco.dollars_per_share, "25.50");
    assert_eq!(csco.fees_dollars, "0.00");
}

// @spec IMPORT-RUN-008
#[test]
fn a_required_cell_failure_routes_to_malformed_never_dropped() {
    let mut grid = actions_grid();
    grid.push(row(&[
        "9",
        "not-a-date",
        "ZZZ",
        "Buy",
        "5",
        "",
        "$10.00",
        "$0.00",
        "$0.00",
    ]));
    grid.push(row(&[
        "10", "1/1/2020", "YYY", "Transfer", "5", "", "$10.00", "$0.00", "$0.00",
    ]));

    let parsed = parse_legacy_tabs(&grid, &sales_grid(), &positions_grid(), &platforms());

    assert_eq!(parsed.actions.len(), 2, "good rows still parse");
    assert_eq!(
        parsed.malformed.len(),
        2,
        "both bad rows are LISTED, not dropped"
    );
    assert!(parsed.malformed[0]
        .reason
        .contains("unparseable required cell"));
    assert!(parsed.malformed[1].reason.contains("unknown Activity"));
    assert_eq!(
        parsed.malformed[0].coord.row, 6,
        "the coordinate points at the sheet row"
    );
}

// @spec IMPORT-RUN-008
#[test]
fn header_is_located_by_name_not_position() {
    // The same tabs with extra furniture above the header still parse.
    let mut grid: Grid = vec![row(&["random banner"]), row(&[""])];
    grid.extend(actions_grid());
    let parsed = parse_legacy_tabs(&grid, &sales_grid(), &positions_grid(), &platforms());
    assert_eq!(parsed.actions.len(), 2);
    assert!(parsed.malformed.is_empty());
}

#[test]
fn share_and_date_primitives_parse_exactly() {
    assert_eq!(parse_shares_to_micro("834"), Some(MicroShares(834_000_000)));
    assert_eq!(parse_shares_to_micro("0.5"), Some(MicroShares(500_000)));
    assert_eq!(
        parse_shares_to_micro("1,234"),
        Some(MicroShares(1_234_000_000))
    );
    assert_eq!(parse_shares_to_micro("#DIV/0!"), None);
    assert_eq!(parse_shares_to_micro(""), None);
    assert_eq!(parse_legacy_date("1/1/2015"), Some(Date(16_436)));
    assert_eq!(parse_legacy_date("11/15/2021"), Some(Date(18_946)));
    assert_eq!(
        parse_legacy_date("2015-01-01"),
        None,
        "ISO is not the legacy format"
    );
}

// @spec IMPORT-MAP-007
#[test]
fn a_never_filled_price_cell_derives_the_unit_price_from_recorded_proceeds() {
    // Four whole-share sale rows exercising the never-filled-price rule:
    //  r2: $0 price, SF $998.00 + $2.00 fees → gross $1,000.00 exact over 10 sh
    //      → the TRUE execution price $100.00, fees untouched;
    //  r3: blank price, SF $999.99, no fees → inexact over 10 sh → ceiling
    //      $100.00 with the 1¢ remainder folded into fees, so reconstructed
    //      proceeds (10 × $100.00 − $0.01) equal the recorded $999.99 to the cent;
    //  r4: a PRESENT $/share governs — the $50.00 price survives even though a
    //      derived `Sales - Fees` cell disagrees;
    //  r5: zero price + zero proceeds stays untouched (the sell-to-cover case
    //      the Vest reconstruction substitutes FMV for).
    let sales: Grid = vec![
        row(&[
            "ID",
            "Date",
            "Stock",
            "Shares Sold",
            "$/share",
            "Cost",
            "Commission",
            "Fees",
            "Sales - Fees",
            "Profit",
        ]),
        row(&[
            "G1", "3/1/2022", "GOOG", "10", "$0.00", "", "", "$2.00", "$998.00", "$0.00",
        ]),
        row(&[
            "G1", "3/2/2022", "GOOG", "10", "", "", "", "", "$999.99", "$0.00",
        ]),
        row(&[
            "G1", "3/3/2022", "GOOG", "10", "$50.00", "", "", "", "$999.00", "$0.00",
        ]),
        row(&[
            "V1-a", "3/4/2022", "GOOG", "10", "$0.00", "", "", "", "$0.00", "$0.00",
        ]),
    ];
    let parsed = parse_legacy_tabs(&actions_grid(), &sales, &positions_grid(), &platforms());
    assert!(
        parsed.malformed.is_empty(),
        "no malformed rows: {:?}",
        parsed.malformed
    );
    assert_eq!(parsed.sales.len(), 4);

    let exact = &parsed.sales[0];
    assert_eq!(
        exact.dollars_per_share, "100.00",
        "exact gross/shares is the true execution price"
    );
    assert_eq!(
        exact.fees_dollars, "2.00",
        "fees untouched on the exact split"
    );

    let ceiled = &parsed.sales[1];
    assert_eq!(
        ceiled.dollars_per_share, "100.00",
        "ceil($999.99 / 10 sh) = $100.00"
    );
    assert_eq!(
        ceiled.fees_dollars, "0.01",
        "the non-negative remainder folds into fees"
    );

    let present = &parsed.sales[2];
    assert_eq!(
        present.dollars_per_share, "50.00",
        "a present price always governs"
    );
    assert_eq!(present.fees_dollars, "0.00");

    let stc = &parsed.sales[3];
    assert_eq!(
        stc.dollars_per_share, "0.00",
        "zero-price zero-proceeds stays the sell-to-cover case"
    );
}

// @spec IMPORT-RECON-007
#[test]
fn the_realized_target_is_the_pretax_profit_sum_not_the_posttax_positions_cell() {
    // Two GOOG sales carry pre-tax Profit $100.00 + $50.50; the legacy Positions
    // "Realized Profit" cell holds a post-tax $90.00. The reconcile target must be
    // the PRE-TAX per-symbol Profit sum, replacing the Positions cell at the
    // boundary; shares and unrealized targets stay the Positions row's.
    let sales: Grid = vec![
        row(&[
            "ID",
            "Date",
            "Stock",
            "Shares Sold",
            "$/share",
            "Cost",
            "Commission",
            "Fees",
            "Sales - Fees",
            "Profit",
        ]),
        row(&[
            "G1", "6/1/2021", "GOOG", "10", "$55.00", "", "", "", "$550.00", "$100.00",
        ]),
        row(&[
            "G1", "7/1/2021", "GOOG", "5", "$60.00", "", "", "", "$300.00", "$50.50",
        ]),
    ];
    let positions: Grid = vec![
        row(&[
            "Stock",
            "Name",
            "Cur Shs",
            "avg $cst/shr",
            "Total Rm Cost",
            "D?",
            "Goal Date",
            "Cur $/s",
            "Current total",
            "",
            "Unr. Profit",
            "Tax",
            "Net Unr. Profit",
            "unr%",
            "",
            "Realized Profit",
        ]),
        row(&[
            "GOOG",
            "Alphabet Inc.",
            "85",
            "$50.00",
            "$4,250.00",
            "",
            "",
            "$60.00",
            "$5,100.00",
            "",
            "$850.00",
            "$0.00",
            "$850.00",
            "20%",
            "",
            "$90.00",
        ]),
    ];
    let parsed = parse_legacy_tabs(&actions_grid(), &sales, &positions, &platforms());
    assert!(
        parsed.malformed.is_empty(),
        "no malformed rows: {:?}",
        parsed.malformed
    );

    assert_eq!(
        parsed.profit_sums.get("GOOG"),
        Some(&Cents(15_050)),
        "ΣProfit = $150.50 pre-tax"
    );
    let goog = parsed
        .positions
        .iter()
        .find(|p| p.symbol == "GOOG")
        .expect("GOOG target row");
    assert_eq!(
        goog.realized_pnl_cents,
        Cents(15_050),
        "the pre-tax sum replaces the post-tax cell"
    );
    assert_eq!(
        goog.shares,
        MicroShares(85_000_000),
        "the share target stays the Positions row"
    );
    assert_eq!(
        goog.unrealized_cents,
        Cents(85_000),
        "the unrealized target stays the Positions row"
    );
}

// @spec IMPORT-RECON-007
#[test]
fn an_unparseable_profit_cell_is_a_malformed_source_row() {
    // The pre-tax realized target cannot tolerate an unreadable Profit: the row
    // routes to malformed (never dropped) rather than summing as zero.
    let sales: Grid = vec![
        row(&[
            "ID",
            "Date",
            "Stock",
            "Shares Sold",
            "$/share",
            "Cost",
            "Commission",
            "Fees",
            "Sales - Fees",
            "Profit",
        ]),
        row(&[
            "G1", "6/1/2021", "GOOG", "10", "$55.00", "", "", "", "$550.00", "#REF!",
        ]),
    ];
    let parsed = parse_legacy_tabs(&actions_grid(), &sales, &positions_grid(), &platforms());
    let bad = parsed
        .malformed
        .iter()
        .find(|m| m.reason.contains("Profit"))
        .expect("the unparseable-Profit row is listed");
    assert_eq!(bad.coord.row, 2, "the coordinate points at the sheet row");
    assert!(
        !parsed.sales.iter().any(|s| s.coord.row == 2),
        "the bad row is not reconstructed"
    );
}
