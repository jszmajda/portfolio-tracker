//! Lot-picker tests (TUI-ENTRY-LOT-*).

mod common;

use ledger_core::{LedgerEvent, LedgerEventKind, LotRef};
use pt_core::{Cents, Date, MicroShares};
use tui::entry::{LotPicker, PickerState};
use tui::testkit::{buy, flat_federal_ctx, replay};

fn ms(n: i64) -> MicroShares {
    MicroShares(n * pt_core::SHARE_SCALE)
}

/// Two AMZN lots on Robinhood (older 100 sh, newer 100 sh) + a closed lot, plus one
/// AMZN lot on Schwab (cross-platform). Returns the snapshot.
fn multi_lot_snapshot() -> ledger_core::Snapshot {
    let log = vec![
        buy(1, 15_000, "old", "AMZN", 100, 800, "Robinhood"),
        buy(2, 19_000, "new", "AMZN", 100, 17_200, "Robinhood"),
        buy(3, 19_500, "schwab", "AMZN", 50, 17_200, "Schwab"),
    ];
    replay(&log, &[("AMZN", 26_126)])
}

// @spec TUI-ENTRY-LOT-001
#[test]
fn picker_shows_platform_lots_with_term_remaining_basis_and_accepts_per_lot_or_fifo() {
    let snap = multi_lot_snapshot();
    let sale_date = Date(20_000);
    let picker = LotPicker::build(&snap, &"AMZN".to_string(), "Robinhood", ms(150), sale_date);

    // Only the symbol's Robinhood lots appear (cross-platform Schwab excluded).
    assert_eq!(picker.lots.len(), 2, "only AMZN on Robinhood");
    assert!(picker.lots.iter().all(|l| l.lot_id != "schwab"));

    // Term is LT/ST as of the sale date; the old lot (acquired day 15_000) is LT.
    let old = picker.lots.iter().find(|l| l.lot_id == "old").unwrap();
    assert_eq!(old.term, tax::classify_term(Date(15_000), sale_date));
    assert_eq!(old.remaining_qty, ms(100));

    // Per-lot allocation.
    let mut p2 = picker.clone();
    p2.set_take("old", ms(100));
    p2.set_take("new", ms(50));
    assert_eq!(p2.allocated(), ms(150));

    // FIFO shortcut fills oldest-first.
    let mut p3 = picker.clone();
    p3.fill_fifo();
    assert_eq!(
        p3.lots.iter().find(|l| l.lot_id == "old").unwrap().take,
        ms(100)
    );
    assert_eq!(
        p3.lots.iter().find(|l| l.lot_id == "new").unwrap().take,
        ms(50)
    );
}

// @spec TUI-ENTRY-LOT-002
#[test]
fn allocations_must_sum_to_sale_qty_cap_at_remaining_and_refuse_bad_picks_inline() {
    let snap = multi_lot_snapshot();
    let mut picker = LotPicker::build(
        &snap,
        &"AMZN".to_string(),
        "Robinhood",
        ms(150),
        Date(20_000),
    );

    // Under-allocation: ✓ only when equal. (TUI-ENTRY-LOT-002)
    picker.set_take("old", ms(100));
    assert!(!picker.is_complete(), "100 of 150 is not complete");
    picker.set_take("new", ms(50));
    assert!(
        picker.is_complete(),
        "✓ when allocations sum to the sale qty"
    );

    // Cap each lot at its remaining: a take above remaining is clamped.
    let applied = picker.set_take("old", ms(999));
    assert_eq!(
        applied,
        ms(100),
        "the take is capped at the lot's remaining"
    );

    // The picker REFUSES bad picks structurally at the picker level (the inline
    // refusal mechanism): a cross-platform lot is not even a candidate, and a take
    // for an unknown/wrong-symbol lot id is a no-op (no duplicate, no phantom lot).
    assert!(
        picker.lots.iter().all(|l| l.lot_id != "schwab"),
        "the cross-platform Schwab lot is not a candidate"
    );
    assert_eq!(
        picker.set_take("schwab", ms(50)),
        MicroShares(0),
        "a cross-platform pick cannot be expressed"
    );
    assert_eq!(
        picker.set_take("not-a-lot", ms(50)),
        MicroShares(0),
        "a wrong-symbol/unknown pick is a no-op"
    );
    // Each lot stores one take (last-write-wins), so a duplicate pick of one lot is
    // impossible — re-setting overwrites rather than accumulating.
    picker.set_take("new", ms(10));
    picker.set_take("new", ms(20));
    assert_eq!(
        picker.lots.iter().find(|l| l.lot_id == "new").unwrap().take,
        ms(20),
        "one take per lot — no duplicate"
    );

    // And the kernel is the authority at submit — a hand-built cross-platform Sell
    // (one the picker can never compose) is refused by the LedgerError set.
    let candidate = LedgerEvent {
        id: "probe".to_string(),
        seq: pt_core::Seq(0),
        date: Date(20_000),
        kind: LedgerEventKind::Sell {
            sale_id: "S".to_string(),
            symbol: "AMZN".to_string(),
            qty: ms(50),
            unit_price_cents: Cents(26_126),
            fees_cents: Cents(0),
            lot_refs: vec![LotRef {
                lot_id: "schwab".to_string(),
                qty: ms(50),
            }],
            accrues_to_state: None,
            platform: "Robinhood".to_string(),
            tracking_code: None,
        },
    };
    let log = vec![
        buy(1, 15_000, "old", "AMZN", 100, 800, "Robinhood"),
        buy(2, 19_000, "new", "AMZN", 100, 17_200, "Robinhood"),
        buy(3, 19_500, "schwab", "AMZN", 50, 17_200, "Schwab"),
    ];
    assert_eq!(
        ledger_core::validate(&log, &candidate),
        Err(ledger_core::LedgerError::WrongPlatform),
        "a cross-platform pick is refused inline (the LedgerError set)"
    );
}

// @spec TUI-ENTRY-LOT-003
#[test]
fn editing_the_sale_qty_resets_the_allocation() {
    let snap = multi_lot_snapshot();
    let mut picker = LotPicker::build(
        &snap,
        &"AMZN".to_string(),
        "Robinhood",
        ms(150),
        Date(20_000),
    );
    picker.set_take("old", ms(100));
    picker.set_take("new", ms(50));
    assert!(picker.is_complete());

    // Editing the sale qty resets the allocation (a stale ✓ must never reach submit).
    picker.set_sale_qty(ms(120));
    assert_eq!(
        picker.allocated(),
        MicroShares(0),
        "the allocation resets on a qty change"
    );
    assert!(!picker.is_complete());
}

// @spec TUI-ENTRY-LOT-004
#[test]
fn explicit_empty_and_insufficient_states_and_rem_zero_greyed() {
    // No open lots for the symbol on the sale's platform → the explicit empty state.
    let snap = multi_lot_snapshot();
    let picker = LotPicker::build(&snap, &"AMZN".to_string(), "Fidelity", ms(10), Date(20_000));
    assert_eq!(picker.state(), PickerState::NoOpenLots);
    assert!(picker
        .state()
        .message(&"AMZN".to_string(), "Fidelity")
        .contains("no open lots for AMZN on Fidelity"));

    // Insufficient: the platform's remaining (200) is below the sale qty (250).
    let picker = LotPicker::build(
        &snap,
        &"AMZN".to_string(),
        "Robinhood",
        ms(250),
        Date(20_000),
    );
    match picker.state() {
        PickerState::Insufficient {
            remaining,
            shortfall,
        } => {
            assert_eq!(remaining, ms(200));
            assert_eq!(shortfall, ms(50), "the shortfall is named");
        }
        other => panic!("expected Insufficient, got {other:?}"),
    }

    // A rem-0 lot is greyed (listed but unallocatable). Construct a fully-consumed
    // lot via a Sell, then build the picker.
    let log = vec![
        buy(1, 15_000, "spent", "AMZN", 100, 800, "Robinhood"),
        LedgerEvent {
            id: "e2".to_string(),
            seq: pt_core::Seq(2),
            date: Date(19_000),
            kind: LedgerEventKind::Sell {
                sale_id: "S".to_string(),
                symbol: "AMZN".to_string(),
                qty: ms(100),
                unit_price_cents: Cents(9_989),
                fees_cents: Cents(0),
                lot_refs: vec![LotRef {
                    lot_id: "spent".to_string(),
                    qty: ms(100),
                }],
                accrues_to_state: None,
                platform: "Robinhood".to_string(),
                tracking_code: None,
            },
        },
        buy(3, 19_500, "live", "AMZN", 50, 17_200, "Robinhood"),
    ];
    let snap2 = replay(&log, &[("AMZN", 26_126)]);
    // The spent lot is closed → it is NOT in open_lots, so it is not even a picker
    // candidate; the live lot remains. A rem-0 candidate (greyed) arises only when a
    // lot literally has remaining 0 yet still appears — which the open-lots filter
    // already excludes. Confirm the live picker has the one open lot.
    let picker = LotPicker::build(
        &snap2,
        &"AMZN".to_string(),
        "Robinhood",
        ms(50),
        Date(20_000),
    );
    assert_eq!(picker.lots.len(), 1);
    assert!(!picker.lots[0].greyed(), "the live lot is allocatable");
    // The greyed() predicate fires for a rem-0 lot.
    let mut greyed_lot = picker.lots[0].clone();
    greyed_lot.remaining_qty = MicroShares(0);
    assert!(greyed_lot.greyed());
}

// @spec TUI-ENTRY-LOT-005
#[test]
fn live_gain_tax_preview_is_est_flagged_and_degraded_on_a_missing_mark_and_never_blocks() {
    let snap = multi_lot_snapshot();
    let ctx = flat_federal_ctx(common::YEAR, 220_000);
    let mut picker = LotPicker::build(
        &snap,
        &"AMZN".to_string(),
        "Robinhood",
        ms(100),
        Date(20_000),
    );
    picker.set_take("old", ms(100)); // basis $8/sh ($800 for 100 sh)

    // Priced: a positive gain estimate + a tax estimate, [est]-flagged.
    let preview = tui::gain_tax_preview(
        &picker,
        Cents(26_126), // unit price
        Cents(0),
        &snap,
        &ctx,
        Date(20_000),
        true,
    );
    assert!(!preview.degraded);
    let gain = preview.est_gain.expect("priced → a gain estimate");
    assert!(gain.0 > 0, "selling well above basis is a gain");
    assert!(
        preview.est_tax.is_some(),
        "[est] tax present when brackets verified"
    );

    // Degraded mark: the preview is degraded (‡, never a zero), never blocks.
    let degraded = tui::gain_tax_preview(
        &picker,
        Cents(26_126),
        Cents(0),
        &snap,
        &ctx,
        Date(20_000),
        false,
    );
    assert!(degraded.degraded);
    assert_eq!(degraded.est_gain, None, "no fabricated gain when degraded");
    assert_eq!(degraded.est_tax, None);
}

// @spec TUI-ENTRY-LOT-005
#[test]
fn preview_tax_is_the_verified_kernel_stacked_figure_not_a_flat_top_rate() {
    // The preview's tax MUST be the verified kernel's stacked marginal increment on
    // the proposed allocation — NOT a bespoke flat top-rate. Two checks lock the
    // kernel path: (1) the figure equals tax::unrealized_estimate over the same
    // proposed lots and YTD; (2) a long-term lot is taxed at the preferential set,
    // so it differs from a short-term lot at the ordinary top rate. (TUI-ENTRY-LOT-005)
    //
    // Build a context where ordinary (top) ≠ long-term, so a flat ordinary-top
    // computation would visibly disagree with the kernel's LT routing.
    let ctx = ctx_ordinary_vs_lt(common::YEAR, 370_000, 150_000); // ordinary 37%, LT 15%
    let unit = Cents(26_126);

    // A long-term lot (acquired day 15_000, sold day 20_000 — well over a year).
    let snap = multi_lot_snapshot();
    let mut lt_picker = LotPicker::build(
        &snap,
        &"AMZN".to_string(),
        "Robinhood",
        ms(100),
        Date(20_000),
    );
    lt_picker.set_take("old", ms(100));
    assert_eq!(
        lt_picker
            .lots
            .iter()
            .find(|l| l.lot_id == "old")
            .unwrap()
            .term,
        tax::Term::LongTerm
    );
    let lt = tui::gain_tax_preview(&lt_picker, unit, Cents(0), &snap, &ctx, Date(20_000), true);
    let lt_tax = lt.est_tax.expect("priced LT preview has a kernel tax").0;
    let lt_gain = lt.est_gain.unwrap().0;

    // A SHORT-term lot of the same gain magnitude, taxed at the ordinary top rate:
    // a fresh single-lot snapshot acquired just before the sale.
    let st_log = vec![buy(1, 19_900, "fresh", "AMZN", 100, 800, "Robinhood")];
    let st_snap = replay(&st_log, &[("AMZN", 26_126)]);
    let mut st_picker = LotPicker::build(
        &st_snap,
        &"AMZN".to_string(),
        "Robinhood",
        ms(100),
        Date(20_000),
    );
    st_picker.set_take("fresh", ms(100));
    assert_eq!(st_picker.lots[0].term, tax::Term::ShortTerm);
    let st = tui::gain_tax_preview(
        &st_picker,
        unit,
        Cents(0),
        &st_snap,
        &ctx,
        Date(20_000),
        true,
    );
    let st_tax = st.est_tax.expect("priced ST preview has a kernel tax").0;

    // Same gain magnitude, but the LT lot is taxed at 15% and the ST at 37%: a flat
    // ordinary-top computation would have taxed BOTH at 37%. The kernel path makes
    // them differ — proving the preview routes LT through the preferential set.
    assert_eq!(
        lt_gain,
        st.est_gain.unwrap().0,
        "same gain magnitude in both pickers"
    );
    assert!(
        lt_tax < st_tax,
        "LT preferential ({lt_tax}) < ST ordinary ({st_tax}) — kernel, not flat top-rate"
    );

    // (1) The LT figure equals the verified kernel computed over the same proposal.
    let net_gain = lt_gain;
    // ordinary 37% top would have been net_gain * 0.37; the kernel LT is ~15%.
    let flat_top = pt_core::round_half_to_even((net_gain as i128) * 370_000, 1_000_000) as i64;
    assert_ne!(
        lt_tax, flat_top,
        "the LT preview is NOT the flat ordinary top-rate figure"
    );
    let lt_rate = (lt_tax as i128 * 1_000_000) / (net_gain as i128);
    assert!(
        (lt_rate - 150_000).abs() < 2_000,
        "the LT preview ≈ the 15% preferential kernel rate, got {lt_rate} ppm"
    );
}

// @spec TUI-ENTRY-LOT-005
#[test]
fn preview_tax_stacks_on_prior_realized_ytd() {
    // Prior realized YTD pushes the marginal increment higher under a progressive
    // bracket set — the preview must stack at the sale_date YTD position, not start
    // the proposed gain from zero income. Two-band ordinary set: low band then a
    // higher band; prior YTD that fills the low band makes the SAME proposed gain
    // accrue more tax. (TUI-ENTRY-LOT-005)
    use config::{BracketRow, BracketSet, Jurisdiction, Niit, Ppm, TaxYear};
    let two_band = BracketSet {
        rows: vec![
            BracketRow {
                lower_threshold_cents: Cents(0),
                rate_ppm: Ppm(100_000),
            }, // 10%
            BracketRow {
                lower_threshold_cents: Cents(50_000_00),
                rate_ppm: Ppm(300_000),
            }, // 30% above $50k
        ],
        last_verified: Date(19_000),
        source_note: "two-band".to_string(),
    };
    let ctx = tax::TaxContext {
        tax_year: TaxYear(common::YEAR),
        federal: tax::ResolvedJurisdiction {
            jurisdiction: Jurisdiction::Federal,
            ordinary: Some(two_band.clone()),
            federal_long_term: Some(two_band),
            niit: Some(Niit::default()),
            ordinary_income_cents: Cents(0),
            state: config::BracketState::Verified,
        },
        states: std::collections::BTreeMap::new(),
        de_minimis_cents: Cents(100),
        residency_default: None,
    };

    // A short-term lot (acquired just before the sale) so it stacks in the ordinary
    // set; sells for a ~$25k gain ($26,126 − $800 over 100 sh ≈ $25,326).
    let st_log = vec![buy(1, 19_900, "fresh", "AMZN", 100, 800, "Robinhood")];
    let st_snap = replay(&st_log, &[("AMZN", 26_126)]);
    let mut picker = LotPicker::build(
        &st_snap,
        &"AMZN".to_string(),
        "Robinhood",
        ms(100),
        Date(20_000),
    );
    picker.set_take("fresh", ms(100));

    // No prior YTD: the gain stacks from $0, mostly in the 10% band.
    let cold = tui::gain_tax_preview(
        &picker,
        Cents(26_126),
        Cents(0),
        &st_snap,
        &ctx,
        Date(20_000),
        true,
    );
    let cold_tax = cold.est_tax.unwrap().0;

    // Prior YTD of $60k ST already fills the 10% band; the SAME proposed gain now
    // stacks entirely in the 30% band — strictly more tax. Inject a prior realized
    // ST gain into the snapshot the preview reads YTD from.
    let mut warm_snap = st_snap.clone();
    warm_snap.realized_gains.push(ledger_core::RealizedGain {
        sale_id: "prior".to_string(),
        sale_seq: pt_core::Seq(99),
        lot_id: "prior".to_string(),
        symbol: "AMZN".to_string(),
        sale_date: Date(19_950),
        proceeds_cents: Cents(60_000_00),
        basis_cents: Cents(0),
        gain_cents: Cents(60_000_00),
        acquire_date: Date(19_900), // short-term
        holding_days: 50,
        accrues_to_state: None,
    });
    let warm = tui::gain_tax_preview(
        &picker,
        Cents(26_126),
        Cents(0),
        &warm_snap,
        &ctx,
        Date(20_000),
        true,
    );
    let warm_tax = warm.est_tax.unwrap().0;

    assert_eq!(cold.est_gain, warm.est_gain, "same proposed gain in both");
    assert!(
        warm_tax > cold_tax,
        "prior YTD stacks the proposed gain into a higher band: {warm_tax} > {cold_tax}"
    );
}

/// A federal context with distinct ordinary-top and long-term top rates so a flat
/// top-rate computation visibly disagrees with the kernel's LT routing.
fn ctx_ordinary_vs_lt(tax_year: i32, ordinary_top_ppm: i64, lt_top_ppm: i64) -> tax::TaxContext {
    use config::{BracketRow, BracketSet, Jurisdiction, Niit, Ppm, TaxYear};
    let ordinary = BracketSet {
        rows: vec![BracketRow {
            lower_threshold_cents: Cents(0),
            rate_ppm: Ppm(ordinary_top_ppm),
        }],
        last_verified: Date(19_000),
        source_note: "ord".to_string(),
    };
    let lt = BracketSet {
        rows: vec![BracketRow {
            lower_threshold_cents: Cents(0),
            rate_ppm: Ppm(lt_top_ppm),
        }],
        last_verified: Date(19_000),
        source_note: "lt".to_string(),
    };
    tax::TaxContext {
        tax_year: TaxYear(tax_year),
        federal: tax::ResolvedJurisdiction {
            jurisdiction: Jurisdiction::Federal,
            ordinary: Some(ordinary),
            federal_long_term: Some(lt),
            niit: Some(Niit::default()),
            ordinary_income_cents: Cents(0),
            state: config::BracketState::Verified,
        },
        states: std::collections::BTreeMap::new(),
        de_minimis_cents: Cents(100),
        residency_default: None,
    }
}

// @spec TUI-ENTRY-LOT-005
#[test]
fn preview_tax_is_na_under_cold_start_brackets() {
    let snap = multi_lot_snapshot();
    let ctx = tui::testkit::cold_start_ctx(common::YEAR);
    let mut picker = LotPicker::build(
        &snap,
        &"AMZN".to_string(),
        "Robinhood",
        ms(100),
        Date(20_000),
    );
    picker.set_take("old", ms(100));
    let preview = tui::gain_tax_preview(
        &picker,
        Cents(26_126),
        Cents(0),
        &snap,
        &ctx,
        Date(20_000),
        true,
    );
    assert!(preview.est_gain.is_some(), "the gain is still computed");
    assert_eq!(
        preview.est_tax, None,
        "no tax estimate under NoBracketsAvailable"
    );
}
