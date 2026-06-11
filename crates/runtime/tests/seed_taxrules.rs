//! The executable half of `docs/runbooks/refresh-tax-brackets.md`: seed/refresh
//! the workbook's tax rules through the REAL validated config write path
//! (`ConfigSheetsAdapter::validated_put` under the advisory write-lock — the
//! same gate the TUI's config flow uses). Idempotent: a re-run replaces the
//! year's rules.
//!
//! The inputs split in two:
//!
//! - **PUBLIC transcription (the `tables` module below).** A bracket LIBRARY
//!   transcribed from primary sources per the runbook: ALL FOUR federal
//!   filing-status table sets and many states' schedules — deliberately more
//!   than any one owner uses, so the checked-in data does not reveal which
//!   filing status or state the owner's seeding actually selects. Every
//!   `BracketSet.source_note` names its publication; every figure is correct
//!   for its named source. Update the library when the runbook runs for a new
//!   year.
//! - **OWNER seed (`owner.local.json`, gitignored).** Filing status, the
//!   ordinary-income estimate, residency state, de-minimis, the platform list,
//!   and the symbol→display-name map are personal: the live seeding reads them
//!   from the repo-root `owner.local.json` and FAILS LOUDLY when it is absent.
//!   The checked-in `owner.local.example.json` documents the shape; the offline
//!   guards below run against the example file. Seeding writes EXACTLY the
//!   subset the owner seed selects (one federal status set + one state).
//!
//! Env-gated like the e2e — it writes the LIVE workbook's config tab:
//!
//! ```sh
//! PT_SEED_TAXRULES=1 \
//! PT_WORKBOOK_ID=<workbook id> \
//! GOOGLE_APPLICATION_CREDENTIALS=<service-account json> \
//! cargo test -p runtime --test seed_taxrules -- --include-ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use config::{
    AliasMap, BracketSet, ConfigStore as _, DeMinimis, DisplayNameMap, FilingStatus, Niit,
    PlatformList, ResidencyEntry, ResidencyTimeline, TaxRules, TaxYear,
};
use pt_core::{Cents, Date, NoopLock};
use runtime::sheets::GoogleSheetsApi;
use runtime::ConfigSheetsAdapter;

// ===========================================================================
// The owner seed: the PERSONAL half of the seeding inputs, read from the
// gitignored repo-root `owner.local.json` (live runs) or the checked-in
// `owner.local.example.json` (offline guards). Every parse failure is loud.
// ===========================================================================

struct OwnerSeed {
    filing_status: FilingStatus,
    ordinary_income_cents: Cents,
    residency_state: String,
    de_minimis_cents: Cents,
    platforms: Vec<String>,
    display_names: DisplayNameMap,
}

fn parse_owner_seed(json: &str, what: &str) -> OwnerSeed {
    let v: serde_json::Value =
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{what}: invalid JSON: {e}"));
    let obj = v.as_object().unwrap_or_else(|| panic!("{what}: must be a JSON object"));
    let str_field = |name: &str| -> &str {
        obj.get(name)
            .and_then(|x| x.as_str())
            .unwrap_or_else(|| panic!("{what}: missing/invalid `{name}` (string)"))
    };
    let cents_field = |name: &str| -> Cents {
        Cents(
            obj.get(name)
                .and_then(|x| x.as_i64())
                .unwrap_or_else(|| panic!("{what}: missing/invalid `{name}` (integer cents)")),
        )
    };
    let filing_status = match str_field("filing_status") {
        "Single" => FilingStatus::Single,
        "MarriedFilingJointly" => FilingStatus::MarriedFilingJointly,
        "MarriedFilingSeparately" => FilingStatus::MarriedFilingSeparately,
        "HeadOfHousehold" => FilingStatus::HeadOfHousehold,
        other => panic!("{what}: unknown filing_status `{other}`"),
    };
    let platforms = obj
        .get("platforms")
        .and_then(|x| x.as_array())
        .unwrap_or_else(|| panic!("{what}: missing `platforms` (array of strings)"))
        .iter()
        .map(|p| {
            p.as_str()
                .unwrap_or_else(|| panic!("{what}: platforms entries must be strings"))
                .to_string()
        })
        .collect();
    let names: BTreeMap<String, String> = obj
        .get("display_names")
        .and_then(|x| x.as_object())
        .unwrap_or_else(|| panic!("{what}: missing `display_names` (object)"))
        .iter()
        .map(|(k, val)| {
            let n = val
                .as_str()
                .unwrap_or_else(|| panic!("{what}: display_names.{k} must be a string"));
            (k.clone(), n.to_string())
        })
        .collect();
    OwnerSeed {
        filing_status,
        ordinary_income_cents: cents_field("ordinary_income_cents"),
        residency_state: str_field("residency_state").to_uppercase(),
        de_minimis_cents: cents_field("de_minimis_cents"),
        platforms,
        display_names: DisplayNameMap::new(names),
    }
}

fn load_owner_seed(file_name: &str) -> OwnerSeed {
    // Repo root, two levels above this crate's manifest dir.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(file_name);
    let json = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — the owner seed file is required (copy owner.local.example.json \
             to owner.local.json and fill in real values; see the module doc)",
            path.display()
        )
    });
    parse_owner_seed(&json, file_name)
}

/// The checked-in template with FICTIONAL values — what the offline guards run
/// against, so CI needs no personal data.
fn example_owner_seed() -> OwnerSeed {
    load_owner_seed("owner.local.example.json")
}

// ===========================================================================
// The 2026 bracket LIBRARY: the PUBLIC transcription. All four federal filing
// statuses and many states, each set carrying its primary-source provenance.
// The library is intentionally a superset of what any seeding run writes; the
// owner seed selects the subset. Money is integer cents; rates are ppm
// (1% = 10_000 ppm), so e.g. NJ's 5.525% is exactly 55_250.
// ===========================================================================
mod tables {
    use config::{BracketRow, BracketSet, FilingStatus, Niit, Ppm};
    use pt_core::{Cents, Date};

    /// 2026-06-11 (the transcription/verification date) as days since 1970-01-01.
    pub const VERIFIED: Date = Date(20_615);

    pub const ALL_STATUSES: [FilingStatus; 4] = [
        FilingStatus::Single,
        FilingStatus::MarriedFilingJointly,
        FilingStatus::MarriedFilingSeparately,
        FilingStatus::HeadOfHousehold,
    ];

    /// Every state code the 2026 library carries (graduated, flat, and
    /// no-income-tax entries alike). `state_2026` returns `Some` exactly for
    /// these.
    pub const STATES_2026: [&str; 22] = [
        "DC", "NJ", "CA", "NY", "VA", "MD", "MA", "MN", "OR", "GA", "NC", "IL", "PA", // taxed
        "TX", "FL", "WA", "NV", "TN", "SD", "WY", "AK", "NH", // no tax on ordinary income
    ];

    fn rows(pairs: &[(i64, i64)]) -> Vec<BracketRow> {
        pairs
            .iter()
            .map(|&(cents, ppm)| BracketRow {
                lower_threshold_cents: Cents(cents),
                rate_ppm: Ppm(ppm),
            })
            .collect()
    }

    fn set(pairs: &[(i64, i64)], source_note: &str) -> BracketSet {
        BracketSet { rows: rows(pairs), last_verified: VERIFIED, source_note: source_note.to_string() }
    }

    // -----------------------------------------------------------------------
    // Federal, tax year 2026 — IRS Rev. Proc. 2025-32
    // (https://www.irs.gov/pub/irs-drop/rp-25-32.pdf), verified against the PDF
    // on the VERIFIED date. §4.01 ordinary tables 1–4; §4.03 maximum zero-rate /
    // 15%-rate amounts; NIIT is statutory (IRC §1411, not inflation-adjusted).
    // -----------------------------------------------------------------------

    /// The complete federal table set for one filing status:
    /// `(ordinary, long_term, niit)`.
    pub fn federal_2026(status: FilingStatus) -> (BracketSet, BracketSet, Niit) {
        let ordinary = match status {
            FilingStatus::MarriedFilingJointly => set(
                &[
                    (0, 100_000),          // 10%   $0
                    (2_480_000, 120_000),  // 12%   over $24,800
                    (10_080_000, 220_000), // 22%   over $100,800
                    (21_140_000, 240_000), // 24%   over $211,400
                    (40_355_000, 320_000), // 32%   over $403,550
                    (51_245_000, 350_000), // 35%   over $512,450
                    (76_870_000, 370_000), // 37%   over $768,700
                ],
                "IRS Rev. Proc. 2025-32 §4.01 Table 1 (MFJ)",
            ),
            FilingStatus::HeadOfHousehold => set(
                &[
                    (0, 100_000),          // 10%   $0
                    (1_770_000, 120_000),  // 12%   over $17,700
                    (6_745_000, 220_000),  // 22%   over $67,450
                    (10_570_000, 240_000), // 24%   over $105,700
                    (20_175_000, 320_000), // 32%   over $201,750
                    (25_620_000, 350_000), // 35%   over $256,200
                    (64_060_000, 370_000), // 37%   over $640,600
                ],
                "IRS Rev. Proc. 2025-32 §4.01 Table 2 (HoH)",
            ),
            FilingStatus::Single => set(
                &[
                    (0, 100_000),          // 10%   $0
                    (1_240_000, 120_000),  // 12%   over $12,400
                    (5_040_000, 220_000),  // 22%   over $50,400
                    (10_570_000, 240_000), // 24%   over $105,700
                    (20_177_500, 320_000), // 32%   over $201,775
                    (25_622_500, 350_000), // 35%   over $256,225
                    (64_060_000, 370_000), // 37%   over $640,600
                ],
                "IRS Rev. Proc. 2025-32 §4.01 Table 3 (Single)",
            ),
            FilingStatus::MarriedFilingSeparately => set(
                &[
                    (0, 100_000),          // 10%   $0
                    (1_240_000, 120_000),  // 12%   over $12,400
                    (5_040_000, 220_000),  // 22%   over $50,400
                    (10_570_000, 240_000), // 24%   over $105,700
                    (20_177_500, 320_000), // 32%   over $201,775
                    (25_622_500, 350_000), // 35%   over $256,225
                    (38_435_000, 370_000), // 37%   over $384,350
                ],
                "IRS Rev. Proc. 2025-32 §4.01 Table 4 (MFS)",
            ),
        };
        // §4.03: 0% up to the Maximum Zero Rate Amount, 15% up to the Maximum
        // 15-Percent Rate Amount, 20% above.
        let long_term = match status {
            FilingStatus::MarriedFilingJointly => set(
                &[
                    (0, 0),                // 0%    $0
                    (9_890_000, 150_000),  // 15%   over $98,900
                    (61_370_000, 200_000), // 20%   over $613,700
                ],
                "IRS Rev. Proc. 2025-32 §4.03 (MFJ zero/15% amounts)",
            ),
            FilingStatus::MarriedFilingSeparately => set(
                &[
                    (0, 0),                // 0%    $0
                    (4_945_000, 150_000),  // 15%   over $49,450
                    (30_685_000, 200_000), // 20%   over $306,850
                ],
                "IRS Rev. Proc. 2025-32 §4.03 (MFS zero/15% amounts)",
            ),
            FilingStatus::HeadOfHousehold => set(
                &[
                    (0, 0),                // 0%    $0
                    (6_620_000, 150_000),  // 15%   over $66,200
                    (57_960_000, 200_000), // 20%   over $579,600
                ],
                "IRS Rev. Proc. 2025-32 §4.03 (HoH zero/15% amounts)",
            ),
            FilingStatus::Single => set(
                &[
                    (0, 0),                // 0%    $0
                    (4_945_000, 150_000),  // 15%   over $49,450
                    (54_550_000, 200_000), // 20%   over $545,500
                ],
                "IRS Rev. Proc. 2025-32 §4.03 (single/'all other' zero/15% amounts)",
            ),
        };
        // NIIT: statutory 3.8% on net investment income above the MAGI
        // threshold — $250,000 MFJ / $125,000 MFS / $200,000 Single & HoH.
        let niit = Niit {
            rate_ppm: Ppm(38_000),
            magi_threshold_cents: Cents(match status {
                FilingStatus::MarriedFilingJointly => 25_000_000,
                FilingStatus::MarriedFilingSeparately => 12_500_000,
                FilingStatus::Single | FilingStatus::HeadOfHousehold => 20_000_000,
            }),
        };
        (ordinary, long_term, niit)
    }

    // -----------------------------------------------------------------------
    // States, tax year 2026. Some states key their schedule on filing status
    // (NJ, CA, NY, MD, MN, OR); the rest are status-independent. Where a
    // state's 2026 indexed figures were unpublished on the VERIFIED date, the
    // latest published year is transcribed and the source_note says so.
    // No-income-tax states carry a single flat 0% row so the validation gate
    // (non-empty, $0 first) still covers them.
    // -----------------------------------------------------------------------

    /// The 2026 schedule for one `(state, filing status)`. `None` for a state
    /// the library does not carry (the seeding fails loudly on `None`).
    pub fn state_2026(code: &str, status: FilingStatus) -> Option<BracketSet> {
        use FilingStatus as F;
        let single_shape = matches!(status, F::Single | F::MarriedFilingSeparately);
        let s = match code {
            // DC — status-independent. Rate schedule for tax years beginning
            // after 12/31/2021, still in effect for 2026.
            "DC" => set(
                &[
                    (0, 40_000),            // 4.00%   $0
                    (1_000_000, 60_000),    // 6.00%   over $10,000
                    (4_000_000, 65_000),    // 6.50%   over $40,000
                    (6_000_000, 85_000),    // 8.50%   over $60,000
                    (25_000_000, 92_500),   // 9.25%   over $250,000
                    (50_000_000, 97_500),   // 9.75%   over $500,000
                    (100_000_000, 107_500), // 10.75%  over $1,000,000
                ],
                "DC OTR individual income tax rates (tax years after 12/31/2021), D.C. Code §47-1806.03",
            ),
            // NJ — gross income tax, two statutory tables (unchanged since
            // 2020): Table A for Single/MFS, Table B for MFJ/HoH/QSS.
            "NJ" if single_shape => set(
                &[
                    (0, 14_000),           // 1.4%    $0
                    (2_000_000, 17_500),   // 1.75%   over $20,000
                    (3_500_000, 35_000),   // 3.5%    over $35,000
                    (4_000_000, 55_250),   // 5.525%  over $40,000
                    (7_500_000, 63_700),   // 6.37%   over $75,000
                    (50_000_000, 89_700),  // 8.97%   over $500,000
                    (100_000_000, 107_500), // 10.75% over $1,000,000
                ],
                "NJ Division of Taxation Tax Rate Schedules (2020 and after), Table A (Single; Married/CU filing separately); N.J.S.A. 54A:2-1",
            ),
            "NJ" => set(
                &[
                    (0, 14_000),           // 1.4%    $0
                    (2_000_000, 17_500),   // 1.75%   over $20,000
                    (5_000_000, 24_500),   // 2.45%   over $50,000
                    (7_000_000, 35_000),   // 3.5%    over $70,000
                    (8_000_000, 55_250),   // 5.525%  over $80,000
                    (15_000_000, 63_700),  // 6.37%   over $150,000
                    (50_000_000, 89_700),  // 8.97%   over $500,000
                    (100_000_000, 107_500), // 10.75% over $1,000,000
                ],
                "NJ Division of Taxation Tax Rate Schedules (2020 and after), Table B (Married/CU filing jointly; Head of household; Qualifying widow(er)/surviving CU partner); N.J.S.A. 54A:2-1",
            ),
            // CA — FTB 2025 rate schedules (CA indexes in the fall of the tax
            // year; 2026 figures unpublished as of 2026-06). The 1% Mental
            // Health Services Tax (R&TC §17043, all statuses, taxable income
            // over $1,000,000) is merged into the rows, so the top band reads
            // 13.3%.
            "CA" if single_shape => set(
                &[
                    (0, 10_000),            // 1%     $0
                    (1_107_900, 20_000),    // 2%     over $11,079
                    (2_626_400, 40_000),    // 4%     over $26,264
                    (4_145_200, 60_000),    // 6%     over $41,452
                    (5_754_200, 80_000),    // 8%     over $57,542
                    (7_272_400, 93_000),    // 9.3%   over $72,724
                    (37_147_900, 103_000),  // 10.3%  over $371,479
                    (44_577_100, 113_000),  // 11.3%  over $445,771
                    (74_295_300, 123_000),  // 12.3%  over $742,953
                    (100_000_000, 133_000), // 13.3%  over $1,000,000 (12.3% + 1% MHST)
                ],
                "CA FTB 2025 Tax Rate Schedule X (Single; Married/RDP filing separately) — 2026 indexing unpublished as of 2026-06; plus 1% Mental Health Services Tax over $1,000,000 (R&TC §17043)",
            ),
            "CA" if status == F::MarriedFilingJointly => set(
                &[
                    (0, 10_000),            // 1%     $0
                    (2_215_800, 20_000),    // 2%     over $22,158
                    (5_252_800, 40_000),    // 4%     over $52,528
                    (8_290_400, 60_000),    // 6%     over $82,904
                    (11_508_400, 80_000),   // 8%     over $115,084
                    (14_544_800, 93_000),   // 9.3%   over $145,448
                    (74_295_800, 103_000),  // 10.3%  over $742,958
                    (89_154_200, 113_000),  // 11.3%  over $891,542
                    (100_000_000, 123_000), // 12.3%  over $1,000,000 (11.3% + 1% MHST)
                    (148_590_600, 133_000), // 13.3%  over $1,485,906 (12.3% + 1% MHST)
                ],
                "CA FTB 2025 Tax Rate Schedule Y (Married/RDP filing jointly; Qualifying surviving spouse/RDP) — 2026 indexing unpublished as of 2026-06; plus 1% Mental Health Services Tax over $1,000,000 (R&TC §17043)",
            ),
            "CA" => set(
                &[
                    (0, 10_000),            // 1%     $0
                    (2_217_300, 20_000),    // 2%     over $22,173
                    (5_253_000, 40_000),    // 4%     over $52,530
                    (6_771_600, 60_000),    // 6%     over $67,716
                    (8_380_500, 80_000),    // 8%     over $83,805
                    (9_899_000, 93_000),    // 9.3%   over $98,990
                    (50_520_800, 103_000),  // 10.3%  over $505,208
                    (60_625_100, 113_000),  // 11.3%  over $606,251
                    (100_000_000, 123_000), // 12.3%  over $1,000,000 (11.3% + 1% MHST)
                    (101_041_700, 133_000), // 13.3%  over $1,010,417 (12.3% + 1% MHST)
                ],
                "CA FTB 2025 Tax Rate Schedule Z (Head of household) — 2026 indexing unpublished as of 2026-06; plus 1% Mental Health Services Tax over $1,000,000 (R&TC §17043)",
            ),
            // NY — thresholds are statutory (per the official 2025 IT-201-I
            // rate schedule); 2026 RATES apply the enacted FY2026 budget cut
            // (S.3009-C Part A, 2025): bottom five rates each −0.1pp for tax
            // year 2026. NYS DTF had not yet published the 2026 schedule as of
            // the VERIFIED date — rates derived from the statute as amended.
            "NY" if single_shape => set(
                &[
                    (0, 39_000),              // 3.9%   $0
                    (850_000, 44_000),        // 4.4%   over $8,500
                    (1_170_000, 51_500),      // 5.15%  over $11,700
                    (1_390_000, 54_000),      // 5.4%   over $13,900
                    (8_065_000, 59_000),      // 5.9%   over $80,650
                    (21_540_000, 68_500),     // 6.85%  over $215,400
                    (107_755_000, 96_500),    // 9.65%  over $1,077,550
                    (500_000_000, 103_000),   // 10.3%  over $5,000,000
                    (2_500_000_000, 109_000), // 10.9%  over $25,000,000
                ],
                "NY Tax Law §601 as amended by the FY2026 budget (S.3009-C Part A, 2025: bottom-five rates −0.1pp for TY2026); thresholds per IT-201-I (2025) schedule (Single and married filing separately) — DTF's 2026 schedule unpublished as of 2026-06",
            ),
            "NY" if status == F::MarriedFilingJointly => set(
                &[
                    (0, 39_000),              // 3.9%   $0
                    (1_715_000, 44_000),      // 4.4%   over $17,150
                    (2_360_000, 51_500),      // 5.15%  over $23,600
                    (2_790_000, 54_000),      // 5.4%   over $27,900
                    (16_155_000, 59_000),     // 5.9%   over $161,550
                    (32_320_000, 68_500),     // 6.85%  over $323,200
                    (215_535_000, 96_500),    // 9.65%  over $2,155,350
                    (500_000_000, 103_000),   // 10.3%  over $5,000,000
                    (2_500_000_000, 109_000), // 10.9%  over $25,000,000
                ],
                "NY Tax Law §601 as amended by the FY2026 budget (S.3009-C Part A, 2025: bottom-five rates −0.1pp for TY2026); thresholds per IT-201-I (2025) schedule (Married filing jointly and qualifying surviving spouse) — DTF's 2026 schedule unpublished as of 2026-06",
            ),
            "NY" => set(
                &[
                    (0, 39_000),              // 3.9%   $0
                    (1_280_000, 44_000),      // 4.4%   over $12,800
                    (1_765_000, 51_500),      // 5.15%  over $17,650
                    (2_090_000, 54_000),      // 5.4%   over $20,900
                    (10_765_000, 59_000),     // 5.9%   over $107,650
                    (26_930_000, 68_500),     // 6.85%  over $269,300
                    (161_645_000, 96_500),    // 9.65%  over $1,616,450
                    (500_000_000, 103_000),   // 10.3%  over $5,000,000
                    (2_500_000_000, 109_000), // 10.9%  over $25,000,000
                ],
                "NY Tax Law §601 as amended by the FY2026 budget (S.3009-C Part A, 2025: bottom-five rates −0.1pp for TY2026); thresholds per IT-201-I (2025) schedule (Head of household) — DTF's 2026 schedule unpublished as of 2026-06",
            ),
            // VA — status-independent, statutory and unchanged since 1990.
            "VA" => set(
                &[
                    (0, 20_000),         // 2%      $0
                    (300_000, 30_000),   // 3%      over $3,000
                    (500_000, 50_000),   // 5%      over $5,000
                    (1_700_000, 57_500), // 5.75%   over $17,000
                ],
                "Va. Code §58.1-320; Virginia Tax rate schedule (all filing statuses; unchanged since 1990)",
            ),
            // MD — Comptroller Tax Rate Schedules I & II for tax years 2025+
            // (HB 352, 2025, added the 6.25%/6.5% brackets). State tax only:
            // Maryland's mandatory county/local income tax (≈2.25–3.3%) is NOT
            // modeled here.
            "MD" if single_shape => set(
                &[
                    (0, 20_000),            // 2%     $0
                    (100_000, 30_000),      // 3%     over $1,000
                    (200_000, 40_000),      // 4%     over $2,000
                    (300_000, 47_500),      // 4.75%  over $3,000
                    (10_000_000, 50_000),   // 5%     over $100,000
                    (12_500_000, 52_500),   // 5.25%  over $125,000
                    (15_000_000, 55_000),   // 5.5%   over $150,000
                    (25_000_000, 57_500),   // 5.75%  over $250,000
                    (50_000_000, 62_500),   // 6.25%  over $500,000
                    (100_000_000, 65_000),  // 6.5%   over $1,000,000
                ],
                "MD Comptroller Tax Rate Schedule I (Single; MFS; dependent), tax years 2025+ per HB 352 (2025); county piggyback income tax not included",
            ),
            "MD" => set(
                &[
                    (0, 20_000),            // 2%     $0
                    (100_000, 30_000),      // 3%     over $1,000
                    (200_000, 40_000),      // 4%     over $2,000
                    (300_000, 47_500),      // 4.75%  over $3,000
                    (15_000_000, 50_000),   // 5%     over $150,000
                    (17_500_000, 52_500),   // 5.25%  over $175,000
                    (22_500_000, 55_000),   // 5.5%   over $225,000
                    (30_000_000, 57_500),   // 5.75%  over $300,000
                    (60_000_000, 62_500),   // 6.25%  over $600,000
                    (120_000_000, 65_000),  // 6.5%   over $1,200,000
                ],
                "MD Comptroller Tax Rate Schedule II (MFJ; HoH; Qualifying surviving spouse), tax years 2025+ per HB 352 (2025); county piggyback income tax not included",
            ),
            // MA — 5% flat (M.G.L. c.62 §4) plus the 4% surtax on taxable
            // income over the inflation-certified threshold (Mass. Const.
            // amend. art. XLIV): $1,107,750 for 2026 (per DOR 2026 Form 1-ES).
            // Status-independent.
            "MA" => set(
                &[
                    (0, 50_000),           // 5%   $0
                    (110_775_000, 90_000), // 9%   over $1,107,750 (5% + 4% surtax)
                ],
                "MA DOR: 5% rate (M.G.L. c.62 §4) + 4% surtax over the 2026 certified threshold $1,107,750 (Mass. Const. amend. art. XLIV; DOR 2026 Form 1-ES)",
            ),
            // MN — DoR-certified 2026 brackets (press release 2025-12-16),
            // four distinct status schedules.
            "MN" => {
                let (t1, t2, t3) = match status {
                    F::Single => (3_331_000, 10_943_000, 20_315_000), // $33,310 / $109,430 / $203,150
                    F::MarriedFilingJointly => (4_870_000, 19_348_000, 33_793_000), // $48,700 / $193,480 / $337,930
                    F::MarriedFilingSeparately => (2_435_000, 9_674_000, 16_896_500), // $24,350 / $96,740 / $168,965
                    F::HeadOfHousehold => (4_101_000, 16_480_000, 27_006_000), // $41,010 / $164,800 / $270,060
                };
                set(
                    &[
                        (0, 53_500),  // 5.35%
                        (t1, 68_000), // 6.8%
                        (t2, 78_500), // 7.85%
                        (t3, 98_500), // 9.85%
                    ],
                    "MN Dept. of Revenue 2026 income tax brackets (press release 2025-12-16, indexed per M.S. 290.06)",
                )
            }
            // OR — DoR 2025 rate charts S/J (sub-$125k/$250k thresholds index
            // annually per ORS 316.037; the 2026 charts were unpublished as of
            // 2026-06). Chart S: Single/MFS; Chart J: MFJ/HoH/QSS.
            "OR" if single_shape => set(
                &[
                    (0, 47_500),          // 4.75%   $0
                    (440_000, 67_500),    // 6.75%   over $4,400
                    (1_110_000, 87_500),  // 8.75%   over $11,100
                    (12_500_000, 99_000), // 9.9%    over $125,000
                ],
                "OR Dept. of Revenue 2025 tax rate Chart S (Single; MFS) — 2026 indexing unpublished as of 2026-06; ORS 316.037",
            ),
            "OR" => set(
                &[
                    (0, 47_500),          // 4.75%   $0
                    (880_000, 67_500),    // 6.75%   over $8,800
                    (2_220_000, 87_500),  // 8.75%   over $22,200
                    (25_000_000, 99_000), // 9.9%    over $250,000
                ],
                "OR Dept. of Revenue 2025 tax rate Chart J (MFJ; HoH; Qualifying surviving spouse) — 2026 indexing unpublished as of 2026-06; ORS 316.037",
            ),
            // Flat-rate states (status-independent).
            "GA" => set(
                &[(0, 49_900)], // 4.99% flat
                "GA flat rate 4.99% for TY2026 (HB 463, signed 2026-05-11, retroactive to 2026-01-01; supersedes the 5.09% scheduled step)",
            ),
            "NC" => set(
                &[(0, 39_900)], // 3.99% flat
                "NC flat rate 3.99% for TY2026 (N.C.G.S. §105-153.7 per S.L. 2023-134)",
            ),
            "IL" => set(
                &[(0, 49_500)], // 4.95% flat
                "IL flat rate 4.95% (35 ILCS 5/201; unchanged for 2026)",
            ),
            "PA" => set(
                &[(0, 30_700)], // 3.07% flat
                "PA flat rate 3.07% (72 P.S. §7302; unchanged for 2026)",
            ),
            // No tax on ordinary income — flat 0% rows so the validation gate
            // still covers the entries.
            "TX" => set(&[(0, 0)], "TX: no individual income tax (Tex. Const. art. VIII §24-a)"),
            "FL" => set(&[(0, 0)], "FL: no individual income tax (Fla. Const. art. VII §5)"),
            "WA" => set(
                &[(0, 0)],
                "WA: no tax on ordinary/wage income; WA's separate capital-gains excise tax (RCW 82.87) is NOT modeled here",
            ),
            "NV" => set(&[(0, 0)], "NV: no individual income tax"),
            "TN" => set(&[(0, 0)], "TN: no individual income tax (Hall tax fully repealed effective 2021)"),
            "SD" => set(&[(0, 0)], "SD: no individual income tax"),
            "WY" => set(&[(0, 0)], "WY: no individual income tax"),
            "AK" => set(&[(0, 0)], "AK: no individual income tax"),
            "NH" => set(
                &[(0, 0)],
                "NH: no tax on wage income; the interest & dividends tax was repealed effective 2025-01-01 (RSA 77 sunset)",
            ),
            _ => return None,
        };
        Some(s)
    }
}

/// Tax year 2026 for THIS owner: the federal set for the owner seed's filing
/// status plus the schedule for the owner seed's residency state, both drawn
/// from the `tables` library. Fails loudly if the library has no table for the
/// owner's state (transcribe it per the runbook, then re-run).
fn tax_rules_2026(owner: &OwnerSeed) -> TaxRules {
    let (federal_ordinary, federal_long_term, niit) = tables::federal_2026(owner.filing_status);
    let state_set = tables::state_2026(&owner.residency_state, owner.filing_status)
        .unwrap_or_else(|| {
            panic!(
                "no 2026 bracket table transcribed for state `{}` — add it to the library in \
                 this file per docs/runbooks/refresh-tax-brackets.md",
                owner.residency_state
            )
        });

    let mut state_ordinary = BTreeMap::new();
    state_ordinary.insert(owner.residency_state.clone(), state_set);

    TaxRules {
        tax_year: TaxYear(2026),
        filing_status: owner.filing_status,
        federal_ordinary,
        federal_long_term,
        niit,
        state_ordinary,
        // The owner's ordinary-income estimate (day-job W-2; investment gains
        // stack on top and are computed by the system).
        ordinary_income_cents: owner.ordinary_income_cents,
    }
}

// The runbook's seeding step, against the LIVE workbook config tab. Ignored +
// env-gated; run explicitly per the module doc. Reads the REAL owner seed
// (owner.local.json) and fails loudly when it is absent.
#[test]
#[ignore]
fn seed_2026_tax_rules_into_the_live_workbook() {
    if std::env::var("PT_SEED_TAXRULES").as_deref() != Ok("1") {
        eprintln!("SKIP: set PT_SEED_TAXRULES=1 (+ PT_WORKBOOK_ID, GOOGLE_APPLICATION_CREDENTIALS)");
        return;
    }
    let workbook = std::env::var("PT_WORKBOOK_ID").expect("PT_WORKBOOK_ID");
    let creds = std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
        .expect("GOOGLE_APPLICATION_CREDENTIALS");
    let owner = load_owner_seed("owner.local.json");

    let api = GoogleSheetsApi::from_credentials_file(workbook, &creds).expect("auth");
    let mut cfg = ConfigSheetsAdapter::new(api, sheets_view::TAX_RULES_TAB, None);
    // The seeding run is interactive/owner-driven; the validated_put still rides
    // the validation gate. (The cross-process advisory lock guards the machine's
    // live writers; this one-shot runs while nothing else writes.)
    let lock = NoopLock::new();

    let rules_to_seed = tax_rules_2026(&owner);
    let expected_state_rows = rules_to_seed.state_ordinary[&owner.residency_state].rows.len();
    cfg.put_tax_rules(rules_to_seed, &lock).expect("2026 rules validate + persist");
    cfg.put_de_minimis(DeMinimis(owner.de_minimis_cents), &lock).expect("de-minimis");
    cfg.put_residency(
        ResidencyTimeline::from_entries(vec![ResidencyEntry {
            effective_date: Date(0),
            state_code: owner.residency_state.clone(),
        }])
        .expect("timeline"),
        &lock,
    )
    .expect("residency: founding entry");
    cfg.put_platforms(PlatformList::new(owner.platforms.clone()), &lock)
        .expect("platform suggestions");
    cfg.put_aliases(AliasMap::new(BTreeMap::new()), &lock).expect("aliases");
    cfg.put_display_names(owner.display_names.clone(), &lock)
        .expect("display names (the owner seed's symbol→name map)");

    // Read back through the same adapter: the rules round-trip.
    let data = cfg.load().expect("load back");
    let rules = data.rules_by_year.get(&TaxYear(2026)).expect("2026 present");
    assert_eq!(rules.filing_status, owner.filing_status);
    assert_eq!(rules.federal_ordinary.rows.len(), 7);
    assert_eq!(rules.federal_long_term.rows.len(), 3);
    assert_eq!(rules.state_ordinary[&owner.residency_state].rows.len(), expected_state_rows);
    assert_eq!(rules.ordinary_income_cents, owner.ordinary_income_cents);
    assert_eq!(data.display_names, owner.display_names, "the names round-trip");
    eprintln!(
        "SEEDED: 2026 rules (fed 7, LT 3, {} {}) + de-minimis + residency + platforms + {} display names",
        owner.residency_state,
        expected_state_rows,
        owner.display_names.entries().len()
    );
}

// Offline guard: an owner-seed display-name map persists + reads back through
// the validated config write path against the FAKE primitive — so a seed-shape
// or wire-form regression fails CI rather than the live seeding. Runs against
// the checked-in EXAMPLE seed (fictional values; CI carries no personal data).
// (CONFIG-PLATFORM-003)
// @spec CONFIG-PLATFORM-003
#[test]
fn the_owner_seed_display_names_round_trip_through_the_config_adapter() {
    use runtime::testkit::FakeSheetsApi;
    let owner = example_owner_seed();
    let fake = FakeSheetsApi::new();
    let mut cfg = runtime::ConfigSheetsAdapter::new(
        fake,
        sheets_view::TAX_RULES_TAB,
        Some(config::Settings::default()),
    );
    let lock = NoopLock::new();
    cfg.put_display_names(owner.display_names.clone(), &lock).expect("names persist");
    let data = cfg.load().expect("load back");
    assert_eq!(data.display_names, owner.display_names);
    // The example's both-Alphabet share classes and the private holding are covered.
    assert_eq!(data.display_names.resolve("GOOG"), "Alphabet");
    assert_eq!(data.display_names.resolve("GOOGL"), "Alphabet");
    assert_eq!(data.display_names.resolve("PRIVCO"), "PrivCo (private)");
    // An unmapped symbol degrades to its ticker.
    assert_eq!(data.display_names.resolve("MSFT"), "MSFT");
}

// Offline guard: the figures the EXAMPLE seed selects VALIDATE through config's
// gate (ordered rows, $0 first, combined top rate < 100%) — runs in plain
// `cargo test`, so a typo'd transcription fails CI rather than the live seeding.
#[test]
fn the_2026_figures_pass_the_validation_gate() {
    use config::{ConfigData, InMemoryConfig, Settings};
    let owner = example_owner_seed();
    let mut mem = InMemoryConfig::new(ConfigData::default(), Settings::default());
    let lock = NoopLock::new();
    mem.put_tax_rules(tax_rules_2026(&owner), &lock)
        .expect("the transcribed 2026 figures validate");
}

// Offline guard: EVERY table in the library validates through config's gate —
// each of the 22 states paired with each of the four federal status sets (every
// federal set tops out at 37% + 3.8% NIIT, so each pairing exercises the
// worst-case stacked ceiling for that state). A typo anywhere in the library
// fails CI, not just in the subset the owner happens to seed.
#[test]
fn every_library_table_passes_the_validation_gate() {
    use config::{ConfigData, InMemoryConfig, Settings};
    let lock = NoopLock::new();
    for &status in &tables::ALL_STATUSES {
        for &state in &tables::STATES_2026 {
            let (federal_ordinary, federal_long_term, niit) = tables::federal_2026(status);
            let state_set = tables::state_2026(state, status)
                .unwrap_or_else(|| panic!("library missing {state} for {status:?}"));
            let mut state_ordinary = BTreeMap::new();
            state_ordinary.insert(state.to_string(), state_set);
            let rules = TaxRules {
                tax_year: TaxYear(2026),
                filing_status: status,
                federal_ordinary,
                federal_long_term,
                niit,
                state_ordinary,
                ordinary_income_cents: Cents(20_000_000),
            };
            let mut mem = InMemoryConfig::new(ConfigData::default(), Settings::default());
            mem.put_tax_rules(rules, &lock)
                .unwrap_or_else(|e| panic!("{state} × {status:?} fails the gate: {e}"));
        }
    }
}

// Offline guard: library provenance + shape. Every set carries a source note
// and the verification date; the federal sets differ across statuses where the
// Rev. Proc. tables differ; NJ carries BOTH status tables (each with the exact
// 5.525% band); the no-income-tax states are single flat-0% rows.
#[test]
fn the_library_carries_provenance_and_the_status_distinctions() {
    use config::FilingStatus as F;
    // Provenance on every set.
    for &status in &tables::ALL_STATUSES {
        let (ord, lt, _) = tables::federal_2026(status);
        for s in [&ord, &lt] {
            assert!(!s.source_note.is_empty(), "federal {status:?}: empty source note");
            assert_eq!(s.last_verified, tables::VERIFIED);
        }
        assert_eq!(ord.rows.len(), 7, "federal ordinary {status:?}");
        assert_eq!(lt.rows.len(), 3, "federal LT {status:?}");
        for &state in &tables::STATES_2026 {
            let set = tables::state_2026(state, status).unwrap();
            assert!(!set.source_note.is_empty(), "{state} {status:?}: empty source note");
            assert_eq!(set.last_verified, tables::VERIFIED, "{state} {status:?}");
        }
    }
    // The four federal sets are genuinely distinct where the Rev. Proc. says so:
    // MFS's 37% bracket starts at $384,350, half of MFJ's $768,700.
    let (mfj, mfj_lt, mfj_niit) = tables::federal_2026(F::MarriedFilingJointly);
    let (mfs, mfs_lt, mfs_niit) = tables::federal_2026(F::MarriedFilingSeparately);
    let (single, single_lt, single_niit) = tables::federal_2026(F::Single);
    let (hoh, _, hoh_niit) = tables::federal_2026(F::HeadOfHousehold);
    assert_eq!(mfs.rows.last().unwrap().lower_threshold_cents.0 * 2, mfj.rows.last().unwrap().lower_threshold_cents.0);
    // LT zero-rate amounts: MFS is half of MFJ; Single tops out at $545,500 vs HoH/MFJ.
    assert_eq!(mfs_lt.rows[1].lower_threshold_cents.0 * 2, mfj_lt.rows[1].lower_threshold_cents.0);
    assert_eq!(single_lt.rows[2].lower_threshold_cents, Cents(54_550_000));
    // NIIT MAGI thresholds: 250k MFJ / 125k MFS / 200k Single+HoH.
    assert_eq!(mfj_niit.magi_threshold_cents, Cents(25_000_000));
    assert_eq!(mfs_niit.magi_threshold_cents, Cents(12_500_000));
    assert_eq!(single_niit.magi_threshold_cents, Cents(20_000_000));
    assert_eq!(hoh_niit.magi_threshold_cents, Cents(20_000_000));
    assert_eq!(single.rows.len(), 7);
    assert_eq!(hoh.rows.len(), 7);
    // NJ: separate single/joint tables, both carrying the exact 5.525% band.
    let nj_single = tables::state_2026("NJ", F::Single).unwrap();
    let nj_joint = tables::state_2026("NJ", F::MarriedFilingJointly).unwrap();
    assert_ne!(nj_single.rows, nj_joint.rows, "NJ tables A and B differ");
    for (set, name) in [(&nj_single, "A"), (&nj_joint, "B")] {
        assert!(
            set.rows.iter().any(|r| r.rate_ppm.0 == 55_250),
            "NJ table {name} carries 5.525% exactly (55,250 ppm)"
        );
    }
    // No-income-tax states: a single flat 0% row each.
    for state in ["TX", "FL", "WA", "NV", "TN", "SD", "WY", "AK", "NH"] {
        let set = tables::state_2026(state, F::Single).unwrap();
        assert_eq!(set.rows.len(), 1, "{state}");
        assert_eq!(set.rows[0].lower_threshold_cents, Cents(0), "{state}");
        assert_eq!(set.rows[0].rate_ppm.0, 0, "{state}");
    }
}

// Offline guard: the library refuses what it does not carry, and the seeding
// path's selection is exactly owner-config-driven — an unknown residency state
// must fail loudly rather than seed something else.
#[test]
fn an_uncovered_state_yields_none_from_the_library() {
    assert!(tables::state_2026("ZZ", FilingStatus::Single).is_none());
    assert!(tables::state_2026("PR", FilingStatus::MarriedFilingJointly).is_none());
}
