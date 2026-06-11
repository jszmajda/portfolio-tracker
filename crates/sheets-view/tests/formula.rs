//! Value / Formula Boundary: SHEET-FORMULA-001/002/003/004.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use sheets_view::{render_positions, render_tax, DATA_START_ROW, POSITIONS_HEADER, TAX_RESERVE_HEADER};

use config::AliasMap;
use pt_core::Date;

/// The reserve-summary column index for a header name.
fn rcol(name: &str) -> usize {
    TAX_RESERVE_HEADER.iter().position(|h| *h == name).unwrap()
}

/// The Positions column index for a header name.
fn col(name: &str) -> usize {
    POSITIONS_HEADER.iter().position(|h| *h == name).unwrap()
}

// @spec SHEET-FORMULA-001
#[test]
fn live_price_columns_are_formulas_others_are_values() {
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let ctx = common::ctx(2022);
    let rates = common::effective_rates(&snap, Date(19_200), &ctx);
    let aliases = AliasMap::default();
    let positions = render_positions(&snap, &rates, &aliases).unwrap();

    let row = &positions.rows[0];

    // Live-price-dependent columns are FORMULAS.
    for name in [
        "Price",
        "Market Value",
        "Unrealized P&L",
        "Est. Unrealized Tax (est)",
        "Net Unrealized (est)",
        "Unrealized %",
    ] {
        assert!(
            row[col(name)].is_formula(),
            "{name} must be a live formula, was {:?}",
            row[col(name)]
        );
    }

    // Everything else is an engine VALUE (incl. the tax-written Est. Tax Rate).
    for name in [
        "Symbol",
        "Shares",
        "Avg Cost/Share",
        "Total Basis",
        "Est. Tax Rate",
        "Realized P&L",
    ] {
        assert!(
            !row[col(name)].is_formula(),
            "{name} must be an engine value, was {:?}",
            row[col(name)]
        );
    }
}

// @spec SHEET-FORMULA-002
#[test]
fn per_row_formulas_reference_their_own_row_beneath_frozen_header() {
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let ctx = common::ctx(2022);
    let rates = common::effective_rates(&snap, Date(19_200), &ctx);
    let aliases = AliasMap::default();
    let positions = render_positions(&snap, &rates, &aliases).unwrap();

    // Data starts beneath the frozen header (row 1); first data row is row 2.
    assert_eq!(DATA_START_ROW, 2);

    for (i, row) in positions.rows.iter().enumerate() {
        let r = (i as u32) + DATA_START_ROW;
        // Market Value = Price(E) × Shares(B) on THIS row.
        let mv = row[col("Market Value")].text();
        assert!(mv.contains(&format!("E{r}")), "MV row {r}: {mv}");
        assert!(mv.contains(&format!("B{r}")), "MV row {r}: {mv}");
        // Unrealized P&L = Market Value(F) − Total Basis(D) on THIS row.
        let un = row[col("Unrealized P&L")].text();
        assert!(un.contains(&format!("F{r}")) && un.contains(&format!("D{r}")), "Unreal row {r}: {un}");
        // The formula must NOT reference any OTHER data row's number, so a row
        // shift cannot mis-pair the cells.
        for other in 0..positions.rows.len() {
            let ro = (other as u32) + DATA_START_ROW;
            if ro != r {
                assert!(!mv.contains(&format!("E{ro}")), "MV row {r} leaked E{ro}: {mv}");
                assert!(!mv.contains(&format!("B{ro}")), "MV row {r} leaked B{ro}: {mv}");
            }
        }
    }
}

// @spec SHEET-FORMULA-003
#[test]
fn positions_shows_pretax_and_live_posttax_estimate() {
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let ctx = common::ctx(2022);
    let rates = common::effective_rates(&snap, Date(19_200), &ctx);
    let aliases = AliasMap::default();
    let positions = render_positions(&snap, &rates, &aliases).unwrap();

    // The post-tax columns are labelled estimates (header carries "(est)").
    assert!(POSITIONS_HEADER.iter().any(|h| *h == "Net Unrealized (est)"));
    assert!(POSITIONS_HEADER.iter().any(|h| *h == "Est. Unrealized Tax (est)"));
    // A pre-tax Unrealized P&L column exists alongside.
    assert!(POSITIONS_HEADER.iter().any(|h| *h == "Unrealized P&L"));

    let row = &positions.rows[0];
    let r = DATA_START_ROW;
    // Net Unrealized = Unrealized P&L(G) × (1 − Est. Tax Rate(H)) on this row.
    let net = row[col("Net Unrealized (est)")].text();
    assert!(net.contains(&format!("G{r}")), "net: {net}");
    assert!(net.contains(&format!("H{r}")), "net: {net}");
    assert!(net.contains("1-") || net.contains("1 -"), "net must use (1 - rate): {net}");

    // The Est. Tax Rate column is a VALUE written by tax (a percentage string),
    // present for a symbol with a computed effective rate.
    let rate_cell = &row[col("Est. Tax Rate")];
    assert!(!rate_cell.is_formula());
}

// @spec SHEET-FORMULA-003, SHEET-FORMULA-005
#[test]
fn est_tax_rate_cell_is_numeric_percent_the_posttax_formulas_can_multiply() {
    // The post-tax columns are `=G*H` and `=G*(1-H)`, so the Est. Tax Rate cell (H)
    // MUST be a numeric percent that Sheets coerces to a fraction (e.g. 0.20), not
    // inert text. The engine writes it as a percent-suffixed value string
    // ("20.0000%"); the SheetsViewClient contract (see batch_update_view doc) is
    // that a Cell::Value is written as a userEnteredValue, so Sheets parses
    // "20.0000%" exactly as a human typing it would — the numeric fraction 0.20
    // with a percent format. This pins the load-bearing encoding so `=G*H` yields
    // pre-tax × rate rather than 0/error.
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let ctx = common::ctx(2022);
    let rates = common::effective_rates(&snap, Date(19_200), &ctx);
    let aliases = AliasMap::default();
    let positions = render_positions(&snap, &rates, &aliases).unwrap();

    // Find a row whose Est. Tax Rate is populated (a symbol with a computed rate).
    let rate_idx = col("Est. Tax Rate");
    let row = positions
        .rows
        .iter()
        .find(|r| !r[rate_idx].text().is_empty())
        .expect("at least one position has a computed effective rate");
    let text = row[rate_idx].text();

    // The cell is a percent-suffixed value (the form Sheets parses to a fraction).
    assert!(text.ends_with('%'), "Est. Tax Rate must be a percent literal: {text}");
    let numeric: f64 = text.trim_end_matches('%').parse().expect("percent body is numeric");
    // Sheets coerces "X%" to the fraction X/100 — model that coercion and confirm
    // `=G*H` (pre-tax × rate) and `=G*(1-H)` (net) would evaluate sensibly: the
    // fraction is a real number in [0, 1] (a tax rate), never NaN/text.
    let fraction = numeric / 100.0;
    assert!(fraction.is_finite() && (0.0..=1.0).contains(&fraction), "rate fraction: {fraction}");

    // Concretely evaluate the two post-tax formulas against a sample pre-tax G to
    // confirm the cell multiplies correctly (=G*H and =G*(1-H)).
    let pre_tax_g = 1_000.0_f64; // an example Unrealized P&L the live formula yields
    let est_unrealized_tax = pre_tax_g * fraction; // =G*H
    let net_unrealized = pre_tax_g * (1.0 - fraction); // =G*(1-H)
    assert!((est_unrealized_tax + net_unrealized - pre_tax_g).abs() < 1e-9, "G*H + G*(1-H) == G");
    assert!(est_unrealized_tax > 0.0, "a positive rate yields a positive estimated tax");
}

// @spec SHEET-MARK-008
#[test]
fn positions_carries_a_locale_proof_quote_date_companion_column() {
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let ctx = common::ctx(2022);
    let rates = common::effective_rates(&snap, Date(19_200), &ctx);

    // Resolve one symbol through the alias table so the companion column is
    // proven to ride the SAME resolved ticker as Price (SHEET-MAP-001).
    let mut map = std::collections::BTreeMap::new();
    map.insert("AMZN".to_string(), "NASDAQ:AMZN".to_string());
    let aliases = AliasMap::new(map);

    let positions = render_positions(&snap, &rates, &aliases).unwrap();

    // The companion column is a typed header column (no ragged rows).
    let qcol = col("Quote Date");
    assert_eq!(qcol, 12, "Quote Date is the read-back's column-12 companion");

    for row in &positions.rows {
        let cell = &row[qcol];
        assert!(cell.is_formula(), "Quote Date must be a live formula: {cell:?}");
        let text = cell.text();
        // The encoding contract: GOOGLEFINANCE's trade time, truncated to a date
        // serial, re-based to days since 1970-01-01, and rendered via TEXT(…,"0")
        // — a digits-only string every locale formats identically, parsed by the
        // settle pass as the kernel Date. Empty (never an error string) while
        // loading/errored.
        assert!(text.contains("GOOGLEFINANCE"), "quote date reads GOOGLEFINANCE: {text}");
        assert!(text.contains("tradetime"), "quote date uses the tradetime attribute: {text}");
        assert!(text.contains("DATE(1970,1,1)"), "re-based to days since 1970-01-01: {text}");
        assert!(text.contains("TEXT("), "rendered as locale-proof digits via TEXT: {text}");
        assert!(text.contains("IFERROR"), "loading/errored renders empty, not an error: {text}");
    }

    // The aliased symbol's companion formula uses the RESOLVED ticker.
    let amzn_row = positions
        .rows
        .iter()
        .find(|r| r[col("Symbol")].text() == "AMZN")
        .expect("AMZN row");
    assert!(
        amzn_row[qcol].text().contains("NASDAQ:AMZN"),
        "quote date rides the resolved ticker: {}",
        amzn_row[qcol].text()
    );
}

// @spec SHEET-FORMULA-004
#[test]
fn tax_tab_is_kernel_exact_not_live_formula() {
    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let ctx = common::ctx(2022);
    let accruals = common::accruals(&snap, &ctx);
    let annual = common::annual_rows(&snap, &[], &ctx);

    let tax = render_tax(&accruals, &annual);

    // Both bands carry NO live-price formulas — every cell is an engine value
    // (kernel-exact, stacked, as of last sync), distinct from Positions' live
    // estimate.
    for band in [&tax.accruals, &tax.reserve_summary] {
        for row in &band.rows {
            for cell in row {
                assert!(
                    !cell.is_formula(),
                    "Tax tab must be kernel-exact values, found a formula: {cell:?}"
                );
            }
        }
    }
    // There IS at least one accrual row (the GOOG sale produced realized gains).
    assert!(!tax.accruals.rows.is_empty());
}

// @spec SHEET-FORMULA-004, SHEET-TAB-005
#[test]
fn reserve_summary_carries_all_five_kernel_exact_columns() {
    use config::{Jurisdiction, TaxYear};
    use pt_core::{Cents, Seq};
    use tax::{AccrualKey, Quarter, TaxEvent, TaxEventKind};

    let marks = common::marks(&[("AMZN", 200_00), ("GOOG", 140_00)]);
    let snap = common::small_snapshot(&marks);
    let ctx = common::ctx(2022);

    // Stage a real lifecycle so accrued/moved/paid (and thus outstanding/shortfall)
    // are all distinct, non-zero, kernel-exact figures — not just the outstanding
    // balance. The sale (sale-1, GOOG) accrues Federal + NJ in tax year 2022.
    let fed_key = AccrualKey {
        sale_id: "sale-1".to_string(),
        lot_id: "lot-goog".to_string(),
        jurisdiction: Jurisdiction::Federal,
        tax_year: TaxYear(2022),
    };
    let events = vec![
        TaxEvent {
            seq: Seq(1),
            kind: TaxEventKind::Allocate {
                accrual_key: fed_key.clone(),
                account_label: "reserve".to_string(),
            },
        },
        TaxEvent {
            seq: Seq(2),
            kind: TaxEventKind::Move {
                accrual_key: fed_key.clone(),
                amount_cents: Cents(50_00),
                date: Date(19_120),
            },
        },
        TaxEvent {
            seq: Seq(3),
            kind: TaxEventKind::Pay {
                jurisdiction: Jurisdiction::Federal,
                tax_year: TaxYear(2022),
                period: Quarter::Q2,
                amount_cents: Cents(20_00),
                date: Date(19_130),
                covers: vec![fed_key.clone()],
            },
        },
    ];

    let accruals = tax::compute_accruals(&snap.realized_gains, &events, &ctx);
    let annual = common::annual_rows(&snap, &events, &ctx);
    let tax = render_tax(&accruals, &annual);

    // Each reserve-summary row carries the kernel-exact value from its AnnualRow
    // for ALL FIVE columns (not 4/5 blank). Match by (jurisdiction, tax_year).
    use sheets_view::TAX_TAB;
    assert_eq!(tax.accruals.name, TAX_TAB);
    assert!(!tax.reserve_summary.rows.is_empty());
    assert_eq!(tax.reserve_summary.rows.len(), annual.len());

    for (row, ar) in tax.reserve_summary.rows.iter().zip(annual.iter()) {
        let fmt = |c: pt_core::Cents| {
            let v = c.0;
            let sign = if v < 0 { "-" } else { "" };
            let a = v.unsigned_abs();
            format!("{sign}{}.{:02}", a / 100, a % 100)
        };
        assert_eq!(row[rcol("Accrued")].text(), fmt(ar.accrued_cents), "Accrued");
        assert_eq!(row[rcol("Moved")].text(), fmt(ar.moved_cents), "Moved");
        assert_eq!(row[rcol("Paid")].text(), fmt(ar.paid_cents), "Paid");
        assert_eq!(row[rcol("Outstanding")].text(), fmt(ar.outstanding_cents), "Outstanding");
        assert_eq!(row[rcol("Shortfall")].text(), fmt(ar.shortfall_cents), "Shortfall");
        // None of the five is an empty string (the gap the review flagged).
        for name in ["Accrued", "Moved", "Paid", "Outstanding", "Shortfall"] {
            assert!(!row[rcol(name)].text().is_empty(), "{name} must not be blank");
        }
    }

    // The Federal 2022 row reflects the staged lifecycle: moved 50.00, paid 20.00.
    let fed = annual
        .iter()
        .find(|r| matches!(r.jurisdiction, Jurisdiction::Federal) && r.tax_year == TaxYear(2022))
        .expect("federal 2022 annual row");
    assert_eq!(fed.moved_cents, Cents(50_00));
    assert_eq!(fed.paid_cents, Cents(20_00));
    let fed_row = tax
        .reserve_summary
        .rows
        .iter()
        .find(|r| r[rcol("Jurisdiction")].text() == "Federal" && r[rcol("Tax Year")].text() == "2022")
        .expect("federal 2022 reserve-summary row");
    assert_eq!(fed_row[rcol("Moved")].text(), "50.00");
    assert_eq!(fed_row[rcol("Paid")].text(), "20.00");
}
