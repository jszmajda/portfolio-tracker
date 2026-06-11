//! The owner-mappings glue (`pt::import_app::apply_owner_mappings`): the
//! post-split-frame REWRITE — a legacy row retroactively recorded in post-split
//! units despite a pre-split date converts back to its pre-split frame at the
//! parse boundary, by exact integer arithmetic, with any inexact division a
//! loud error and the true date untouched.

use import::parse::ParsedLegacy;
use import::testkit::{buy_row, sale_row};
use import::KnownCorporateAction;
use pt::import_app::{apply_owner_mappings, OwnerInputs};
use pt_core::{Date, MicroShares};

/// Days since 1970-01-01 for civil `(y, m, d)`, matching the import fixtures.
fn date(y: i32, m: i32, d: i32) -> Date {
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as i64;
    let mm = if m > 12 { m - 12 } else { m };
    let mp = if mm > 2 { mm - 3 } else { mm + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + (d - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Date((era as i64 * 146_097 + doe - 719_468) as i32)
}

fn amzn_20_for_1(on: Date) -> KnownCorporateAction {
    KnownCorporateAction { symbol: "AMZN".to_string(), date: on, ratio_num: 20, ratio_den: 1 }
}

// @spec IMPORT-CORP-007
#[test]
fn a_post_split_framed_row_converts_back_to_its_pre_split_frame_exactly() {
    // The owner declares tranche B1 post-split-framed: its legacy row was
    // retroactively recorded as 2,000 sh @ $100 although its TRUE date (2021-03-01)
    // precedes the 20:1 split (2022-06-06). The rewrite converts qty ÷ 20 and
    // $/share × 20 — 100 sh @ $2,000 — leaving the date untouched, so the replayed
    // Split rescales it exactly once. A sale of B1 dated BEFORE the split shares
    // the frame and converts too; one dated AFTER the split is already in its own
    // row-date frame and is untouched.
    let buy_date = date(2021, 3, 1);
    let mut parsed = ParsedLegacy {
        actions: vec![buy_row("Stock Actions", 2, "B1", "AMZN", 2000, "100.00", "0", buy_date, "schwab")],
        sales: vec![
            sale_row("Stock Sales", 2, "B1@r2", "B1", "AMZN", 400, "110.00", "0", date(2022, 1, 10), "schwab"),
            sale_row("Stock Sales", 3, "B1@r3", "B1", "AMZN", 100, "120.00", "0", date(2023, 4, 1), "schwab"),
        ],
        ..ParsedLegacy::default()
    };
    let inputs = OwnerInputs {
        corporate_actions: vec![amzn_20_for_1(date(2022, 6, 6))],
        post_split_framed_ids: vec!["B1".to_string()],
        ..OwnerInputs::default()
    };

    let notes = apply_owner_mappings(&mut parsed, &inputs).expect("the rewrite is exact");

    let b1 = &parsed.actions[0];
    assert_eq!(b1.qty, MicroShares(100_000_000), "qty ÷ 20: 2,000 → 100 shares");
    assert_eq!(b1.dollars_per_share, "2000.00", "$/share × 20: $100 → $2,000");
    assert_eq!(b1.date, buy_date, "the TRUE date is untouched");

    let pre_split_sale = &parsed.sales[0];
    assert_eq!(pre_split_sale.qty, MicroShares(20_000_000), "a pre-split-dated sale shares the frame");
    assert_eq!(pre_split_sale.dollars_per_share, "2200.00");
    assert_eq!(pre_split_sale.date, date(2022, 1, 10), "its date too is untouched");

    let post_split_sale = &parsed.sales[1];
    assert_eq!(post_split_sale.qty, MicroShares(100_000_000), "a post-split sale is already in frame");
    assert_eq!(post_split_sale.dollars_per_share, "120.00");

    assert!(
        notes.iter().any(|n| n.contains("post-split-framed row converted to pre-split frame")),
        "the conversion is reported, never silent: {notes:?}"
    );
}

// @spec IMPORT-CORP-007
#[test]
fn an_inexact_post_split_frame_division_is_a_loud_error() {
    // 100 shares do not divide by a 3:1 ratio in micro-shares (100,000,000 % 3 ≠ 0):
    // the frame assumption is wrong, and the rewrite must fail loudly rather than
    // round a share count.
    let mut parsed = ParsedLegacy {
        actions: vec![buy_row("Stock Actions", 2, "B1", "AMZN", 100, "99.00", "0", date(2021, 3, 1), "schwab")],
        ..ParsedLegacy::default()
    };
    let inputs = OwnerInputs {
        corporate_actions: vec![KnownCorporateAction {
            symbol: "AMZN".to_string(),
            date: date(2022, 6, 6),
            ratio_num: 3,
            ratio_den: 1,
        }],
        post_split_framed_ids: vec!["B1".to_string()],
        ..OwnerInputs::default()
    };

    let err = apply_owner_mappings(&mut parsed, &inputs).expect_err("inexact division is an error");
    assert!(err.contains("not divisible"), "the error names the failure: {err}");
}
