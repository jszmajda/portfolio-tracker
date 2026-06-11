//! Marks Read-Back: SHEET-MARK-001/002/003/004/005.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use std::collections::BTreeMap;

use sheets_view::testkit::{pass, InMemorySheetsView};
use sheets_view::{
    price_to_cents, read_marks, DegradeReason, Mark, PriceReading, SettleConfig,
};

use pt_core::{Cents, Date};

fn symbols(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

// @spec SHEET-MARK-001
#[test]
fn settle_pass_polls_until_numeric_separate_from_write() {
    let client = InMemorySheetsView::new();
    // Two polls: first AMZN is Loading (transient), then it settles numeric.
    client.set_price_script(vec![
        pass(&[("AMZN", PriceReading::Transient)]),
        pass(&[("AMZN", PriceReading::Numeric { price_usd: 200.0, quote_date: Date(19_150) })]),
    ]);

    let cfg = SettleConfig { max_polls: 8 };
    let prior = BTreeMap::new();
    let produced = read_marks(&client, &symbols(&["AMZN"]), &prior, cfg).unwrap();

    // It polled more than once (settled on the second pass) — a separate read
    // pass, never inline with the formula write.
    assert!(client.read_count() >= 2, "must poll until settled, got {}", client.read_count());
    // The settled numeric mark is recorded.
    let m = produced.marks.get("AMZN").expect("settled mark");
    assert_eq!(m.price_cents, Cents(200_00));
}

// @spec SHEET-MARK-001
#[test]
fn settle_pass_is_bounded_by_max_polls() {
    let client = InMemorySheetsView::new();
    // Always transient — never settles.
    client.set_prices(pass(&[("AMZN", PriceReading::Transient)]));

    let cfg = SettleConfig { max_polls: 3 };
    let prior = BTreeMap::new();
    let _ = read_marks(&client, &symbols(&["AMZN"]), &prior, cfg).unwrap();

    // The poll count is bounded by max_polls (no infinite loop).
    assert_eq!(client.read_count(), 3);
}

// @spec SHEET-MARK-002
#[test]
fn usd_price_converts_to_cents_round_half_to_even() {
    // Exact cents.
    assert_eq!(price_to_cents(200.0).unwrap(), Cents(200_00));
    assert_eq!(price_to_cents(150.25).unwrap(), Cents(150_25));
    // Banker's rounding at a half-cent tie: 1.005 → 1.00 (round to even), and
    // 1.015 → 1.02 (round to even). (price × 100 lands on .5 of a cent)
    assert_eq!(price_to_cents(1.005).unwrap(), Cents(1_00));
    assert_eq!(price_to_cents(1.015).unwrap(), Cents(1_02));
}

// @spec SHEET-MARK-002
#[test]
fn subcent_positive_price_is_degraded_anomaly_not_zero() {
    // A positive price that rounds to 0 Cents is a degraded anomaly, NOT a real
    // zero mark (value must never be silently understated).
    let r = price_to_cents(0.0001);
    assert_eq!(r, Err(DegradeReason::SubCent));

    // Drive it through read_marks: the symbol is recorded degraded, with NO mark.
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[(
        "TINY",
        PriceReading::Numeric { price_usd: 0.0001, quote_date: Date(19_150) },
    )]));
    let produced = read_marks(&client, &symbols(&["TINY"]), &BTreeMap::new(), SettleConfig::default()).unwrap();
    assert!(produced.marks.get("TINY").is_none());
    assert_eq!(produced.degraded.get("TINY"), Some(&DegradeReason::SubCent));
}

// @spec SHEET-MARK-002, SHEET-MARK-007
#[test]
fn zero_and_negative_price_are_degraded_not_a_zero_or_negative_mark() {
    // GOOGLEFINANCE should never return ≤ 0 for a real mark; defensively, an exact
    // 0.0 or a negative reading is a degraded anomaly (SubCent), never a real zero
    // or negative mark — value is never silently understated. Non-finite is a
    // permanent degrade. (SHEET-MARK-002, defensive beyond a strict positive-→0¢.)
    assert_eq!(price_to_cents(0.0), Err(DegradeReason::SubCent));
    assert_eq!(price_to_cents(-5.0), Err(DegradeReason::SubCent));
    assert_eq!(price_to_cents(f64::NAN), Err(DegradeReason::Permanent));
    assert_eq!(price_to_cents(f64::INFINITY), Err(DegradeReason::Permanent));

    // Driven through read_marks: a 0.0 reading records the symbol degraded, NO mark.
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[(
        "ZERO",
        PriceReading::Numeric { price_usd: 0.0, quote_date: Date(19_150) },
    )]));
    let produced =
        read_marks(&client, &symbols(&["ZERO"]), &BTreeMap::new(), SettleConfig::default()).unwrap();
    assert!(produced.marks.get("ZERO").is_none());
    assert_eq!(produced.degraded.get("ZERO"), Some(&DegradeReason::SubCent));
}

// @spec SHEET-MARK-005, SHEET-MARK-006
#[test]
fn transport_failure_propagates_error_does_not_emit_empty_marks() {
    // On a transport failure (the workbook unreachable / offline), read_marks
    // propagates the Err and produces NO marks — it must NOT silently emit an
    // empty marks set (that would look like "all symbols degraded", wrongly nulling
    // live positions). The offline carry-forward is runtime's: it retains its prior
    // cache. (SHEET-MARK-005)
    let client = InMemorySheetsView::new();
    client.set_read_fails(true);
    let mut prior = BTreeMap::new();
    prior.insert("AMZN".to_string(), Mark { price_cents: Cents(180_00), quote_date: Date(19_100) });

    let result = read_marks(&client, &symbols(&["AMZN"]), &prior, SettleConfig::default());
    assert!(result.is_err(), "a transport failure must propagate, not emit empty marks");
}

// @spec SHEET-MARK-003
#[test]
fn transient_keeps_prior_mark_and_retries_degrades_only_after_window() {
    let client = InMemorySheetsView::new();
    // Always transient — never settles within the window.
    client.set_prices(pass(&[("AMZN", PriceReading::Transient)]));

    // A prior good cached mark exists.
    let mut prior = BTreeMap::new();
    prior.insert("AMZN".to_string(), Mark { price_cents: Cents(180_00), quote_date: Date(19_100) });

    let cfg = SettleConfig { max_polls: 4 };
    let produced = read_marks(&client, &symbols(&["AMZN"]), &prior, cfg).unwrap();

    // It retried up to the bounded window, then kept the PRIOR good mark rather
    // than overwriting it with a transient (and did not record it degraded).
    assert_eq!(client.read_count(), 4);
    assert_eq!(
        produced.marks.get("AMZN"),
        Some(&Mark { price_cents: Cents(180_00), quote_date: Date(19_100) })
    );
    assert!(produced.degraded.get("AMZN").is_none());
}

// @spec SHEET-MARK-003
#[test]
fn no_prior_mark_stays_transient_degrades_after_window() {
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[("AMZN", PriceReading::Transient)]));

    // No prior mark.
    let cfg = SettleConfig { max_polls: 2 };
    let produced = read_marks(&client, &symbols(&["AMZN"]), &BTreeMap::new(), cfg).unwrap();

    // After the bounded window with no prior mark → degraded (TimedOut), no mark.
    assert!(produced.marks.get("AMZN").is_none());
    assert_eq!(produced.degraded.get("AMZN"), Some(&DegradeReason::TimedOut));
}

// @spec SHEET-MARK-003
#[test]
fn permanent_error_degrades_immediately_no_prior_carry() {
    let client = InMemorySheetsView::new();
    // Permanent #N/A (ticker-form mismatch) — terminal, settles the loop at once.
    client.set_prices(pass(&[("BADX", PriceReading::Permanent)]));

    let mut prior = BTreeMap::new();
    prior.insert("BADX".to_string(), Mark { price_cents: Cents(10_00), quote_date: Date(19_000) });

    let produced = read_marks(&client, &symbols(&["BADX"]), &prior, SettleConfig::default()).unwrap();
    // A permanent error is degraded (Permanent) — NOT carried from prior (it is a
    // terminal error, not a transient).
    assert!(produced.marks.get("BADX").is_none());
    assert_eq!(produced.degraded.get("BADX"), Some(&DegradeReason::Permanent));
    // A permanent error settles the loop without exhausting the poll window.
    assert!(client.read_count() < SettleConfig::default().max_polls);
}

// @spec SHEET-MARK-004
#[test]
fn mark_stamped_with_googlefinance_quote_date_not_wall_clock() {
    let client = InMemorySheetsView::new();
    // The quote date is 19_100 (the GOOGLEFINANCE quote epoch), distinct from any
    // wall-clock read time.
    client.set_prices(pass(&[(
        "AMZN",
        PriceReading::Numeric { price_usd: 200.0, quote_date: Date(19_100) },
    )]));
    let produced = read_marks(&client, &symbols(&["AMZN"]), &BTreeMap::new(), SettleConfig::default()).unwrap();
    let m = produced.marks.get("AMZN").unwrap();
    assert_eq!(m.quote_date, Date(19_100));
}

// @spec SHEET-MARK-005
#[test]
fn degraded_symbol_absent_from_ledger_marks_for_per_symbol_degradation() {
    let client = InMemorySheetsView::new();
    client.set_prices(pass(&[
        ("AMZN", PriceReading::Numeric { price_usd: 200.0, quote_date: Date(19_150) }),
        ("BADX", PriceReading::Permanent),
    ]));
    let produced = read_marks(
        &client,
        &symbols(&["AMZN", "BADX"]),
        &BTreeMap::new(),
        SettleConfig::default(),
    )
    .unwrap();

    // The ledger Marks map (symbol → Cents) injected into replay carries the good
    // mark and OMITS the degraded one, so ledger-core degrades BADX per-symbol
    // (LEDGER-PNL-007) rather than seeing a zero.
    let ledger_marks = produced.to_ledger_marks();
    assert_eq!(ledger_marks.get("AMZN"), Some(&Cents(200_00)));
    assert!(ledger_marks.get("BADX").is_none());

    // Replaying with these marks degrades BADX's unrealized to None (not zero),
    // confirming the feed into ledger-core's per-symbol degradation.
    let events = vec![
        common::buy(1, 19_000, "lot-amzn", "AMZN", 1_000_000, 100_00),
        common::buy(2, 19_010, "lot-badx", "BADX", 1_000_000, 50_00),
    ];
    let snap = common::replay(&events, &ledger_marks);
    assert!(snap.positions.get("AMZN").unwrap().unrealized_cents.is_some());
    assert!(snap.positions.get("BADX").unwrap().unrealized_cents.is_none());
}
