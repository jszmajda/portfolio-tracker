//! Shared fixtures for the `import` suite: civil-date helpers and the canonical
//! SYNTHETIC legacy workbook scenarios (the AMZN 20:1 split with an RSU vest
//! before and sales after; a clean GOOG buy/sell; a closed PLTR). No `#[test]`s
//! here. The real workbook is never touched.
#![allow(dead_code)]
#![allow(clippy::inconsistent_digit_grouping)]

use config::ResidencyTimeline;
use import::testkit::{
    buy_row, closed_year, founding_residency, position_row, residency, sale_row, split_20_for_1,
    vest_row,
};
use import::{
    ClosedYear, KnownCorporateAction, LegacyActionRow, LegacyPositionRow, LegacySaleRow,
    LegacyWorkbook,
};
use pt_core::Date;

/// Days since 1970-01-01 for civil `(y, m, d)` (proleptic Gregorian) — the same
/// algorithm `config`/`tax` use.
pub fn day(y: i32, m: i32, d: i32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + (d - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era as i64 * 146097 + doe - 719468) as i32
}

/// A `Date` for civil `(y, m, d)`.
pub fn date(y: i32, m: i32, d: i32) -> Date {
    Date(day(y, m, d))
}

pub const PLATFORM: &str = "Schwab";

// ---------------------------------------------------------------------------
// The AMZN 20:1 split scenario (the known-hard case). A 100-share RSU vest at
// $2,000/share FMV in 2021, the 20:1 split on 2022-06-06, then a post-split sale
// of 400 of the resulting 2000 shares at $120/share in 2023. The legacy `Total
// Cost` for the vest was $0 (the double-tax error); the legacy `$/share` column
// held the $2,000 FMV, which the importer recovers.
// ---------------------------------------------------------------------------

/// The AMZN vest row (100 shares @ $2,000 FMV, 2021-03-01).
pub fn amzn_vest() -> LegacyActionRow {
    vest_row(
        "Stock Actions",
        2,
        "AMZN-V1",
        "AMZN",
        100,
        "2000.00",
        date(2021, 3, 1),
        PLATFORM,
    )
}

/// The AMZN 20:1 split (2022-06-06).
pub fn amzn_split() -> KnownCorporateAction {
    split_20_for_1("AMZN", date(2022, 6, 6))
}

/// A post-split AMZN sale: 400 of the 2000 post-split shares @ $120/share,
/// 2023-04-01, specific-ID against the vest tranche. (qty is in the SALE's own
/// post-split row-date share-frame.)
pub fn amzn_sale_post_split() -> LegacySaleRow {
    sale_row(
        "Stock Sales",
        2,
        "AMZN-S1",
        "AMZN-V1",
        "AMZN",
        400,
        "120.00",
        "0",
        date(2023, 4, 1),
        PLATFORM,
    )
}

/// The AMZN `Positions` reconcile target after the vest + split + sale: 1600
/// post-split shares remain. Legacy realized P&L and unrealized are BOTH computed
/// on the legacy `$0` RSU basis — the intended RSU-basis divergence the new model
/// fixes. Legacy realized = 400 × $120 − $0 = $48,000 = 4_800_000 cents; legacy
/// unrealized at a $120 mark = 1600 × $120 − $0 = $192,000 = 19_200_000 cents.
pub fn amzn_position_legacy() -> LegacyPositionRow {
    position_row("AMZN", 1600, 4_800_000, 19_200_000)
}

/// A workbook with ONLY the AMZN vest+split+sale and a founding residency entry.
pub fn amzn_only_workbook() -> LegacyWorkbook {
    LegacyWorkbook {
        actions: vec![amzn_vest()],
        sales: vec![amzn_sale_post_split()],
        positions: vec![amzn_position_legacy()],
        corporate_actions: vec![amzn_split()],
        closed_years: vec![],
        residency: founding_residency("DC", date(2015, 1, 1)),
        founding_residency_asserted: false,
        corrections: Default::default(),
        adjudicated: std::collections::BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// A clean GOOG buy/sell (no RSU, no split): a 100-share buy @ $50, a 20-share
// sale @ $55. Reconstructs faithfully; reconciles as MATCHED (no intended delta).
// ---------------------------------------------------------------------------

pub fn goog_buy() -> LegacyActionRow {
    buy_row(
        "Stock Actions",
        3,
        "GOOG-B1",
        "GOOG",
        100,
        "50.00",
        "0",
        date(2020, 1, 15),
        PLATFORM,
    )
}

pub fn goog_sale() -> LegacySaleRow {
    sale_row(
        "Stock Sales",
        3,
        "GOOG-S1",
        "GOOG-B1",
        "GOOG",
        20,
        "55.00",
        "0",
        date(2021, 6, 1),
        PLATFORM,
    )
}

/// Legacy GOOG `Positions`: 80 shares remain; realized = 20 × ($55 − $50) =
/// $100 = 10_000 cents; unrealized at a $60 mark = 80 × ($60 − $50) = $800.
pub fn goog_position_legacy() -> LegacyPositionRow {
    position_row("GOOG", 80, 10_000, 80_000)
}

// ---------------------------------------------------------------------------
// A combined multi-symbol workbook: GOOG (clean) + AMZN (RSU/split) + a closed
// year. The DC closed year 2023 seeds a combined migration accrual.
// ---------------------------------------------------------------------------

pub fn closed_year_dc_2023() -> ClosedYear {
    // The legacy actual DC tax paid for 2023, e.g. $7,200 = 720_000 cents.
    closed_year(
        config::Jurisdiction::State("DC".to_string()),
        2023,
        720_000,
        "DC Reserve",
    )
}

/// The full combined workbook (GOOG + AMZN + the DC 2023 closed year).
pub fn combined_workbook() -> LegacyWorkbook {
    LegacyWorkbook {
        actions: vec![goog_buy(), amzn_vest()],
        sales: vec![goog_sale(), amzn_sale_post_split()],
        positions: vec![goog_position_legacy(), amzn_position_legacy()],
        corporate_actions: vec![amzn_split()],
        closed_years: vec![closed_year_dc_2023()],
        residency: founding_residency("DC", date(2015, 1, 1)),
        founding_residency_asserted: false,
        corrections: Default::default(),
        adjudicated: std::collections::BTreeMap::new(),
    }
}

/// Marks for the reconciliation's unrealized comparison: GOOG @ $60, AMZN @ $120
/// (post-split), in `Cents`/whole-share.
pub fn combined_marks() -> ledger_core::Marks {
    let mut m = ledger_core::Marks::new();
    m.insert("GOOG".to_string(), pt_core::Cents(6000));
    m.insert("AMZN".to_string(), pt_core::Cents(12000));
    m
}

/// A two-entry residency timeline (NJ before 2018, DC from 2018) for stamping a
/// sale by sale-date.
pub fn nj_then_dc() -> ResidencyTimeline {
    residency(&[(date(2010, 1, 1), "NJ"), (date(2018, 1, 1), "DC")])
}
