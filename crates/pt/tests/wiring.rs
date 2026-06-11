//! The PURE cycle-projection helpers: a held `runtime::CycleOutcome` projected into
//! the entry-point bundles (`summary::SummaryInputs`, `tui::ViewState`). No network
//! — the helpers take an already-held outcome, so the projection is exercised
//! without the live cycle. (the live cycle is the env-gated e2e's job.)
#![allow(clippy::inconsistent_digit_grouping)]

use std::collections::BTreeMap;

use ledger_core::Snapshot;
use pt_core::{Cents, Date};
use reports::{PricedMark, TradingDayKey};
use runtime::{CycleOutcome, MarksCache, SymbolFreshness};
use sheets_view::Mark;

use pt::wiring::{context_for_year, summary_inputs_from_cycle, view_state_from_cycle};

/// A minimal held cycle outcome: one priced symbol (AMZN @ $170), no realized
/// gains, keyed to a trading day. Enough to prove the projection threads every
/// field through without recomputing.
fn outcome() -> CycleOutcome {
    let mut marks = MarksCache::new();
    marks.marks.insert(
        "AMZN".to_string(),
        Mark {
            price_cents: Cents(170_00),
            quote_date: Date(19_180),
        },
    );
    CycleOutcome {
        snapshot: Snapshot::default(),
        accruals: Vec::new(),
        annual_rows: Vec::new(),
        estimates: BTreeMap::new(),
        marks,
        trading_day_key: Some(TradingDayKey(Date(19_180))),
    }
}

#[test]
fn summary_inputs_thread_the_cycle_through_unchanged() {
    let out = outcome();
    let inputs = summary_inputs_from_cycle(
        &out,
        config::BracketState::NoBracketsAvailable,
        2026,
        vec![TradingDayKey(Date(19_180))],
        Date(20_000),
        1_700_000_000,
    );

    // The priced marks projected from the cache (AMZN @ $170, stamped).
    assert_eq!(
        inputs.marks.get("AMZN"),
        Some(&PricedMark {
            price_cents: Cents(170_00),
            quote_epoch: Date(19_180)
        })
    );
    assert_eq!(inputs.trading_day_key, Some(TradingDayKey(Date(19_180))));
    assert_eq!(inputs.tax_year, 2026);
    assert_eq!(
        inputs.bracket_state,
        config::BracketState::NoBracketsAvailable
    );
    assert_eq!(inputs.run_at_epoch_secs, 1_700_000_000);
    // summary computes nothing of its own — the snapshot is the cycle's verbatim.
    assert_eq!(inputs.snapshot, out.snapshot);
}

#[test]
fn view_state_threads_freshness_and_marks_through_unchanged() {
    let out = outcome();
    let mut freshness: BTreeMap<String, SymbolFreshness> = BTreeMap::new();
    freshness.insert(
        "AMZN".to_string(),
        SymbolFreshness::Priced {
            quote_epoch: Date(19_180),
        },
    );
    let mut names = BTreeMap::new();
    names.insert("AMZN".to_string(), "Amazon.com".to_string());
    let view = view_state_from_cycle(
        &out,
        freshness.clone(),
        BTreeMap::new(),
        Vec::new(),
        Vec::new(),
        None,
        config::BracketState::NoBracketsAvailable,
        Vec::new(),
        2026,
        tui::port::Connection::Live,
        Some("16:05".to_string()),
        config::DisplayNameMap::new(names),
    );

    assert_eq!(view.trading_day_key, Some(TradingDayKey(Date(19_180))));
    assert_eq!(view.freshness, freshness);
    assert_eq!(
        view.marks.get("AMZN"),
        Some(&PricedMark {
            price_cents: Cents(170_00),
            quote_epoch: Date(19_180)
        })
    );
    // The binary owns the key→calendar conversion: the formatted as-of threads
    // through, never a raw key int. (TUI-VIEW-NAV-012)
    assert_eq!(
        view.as_of_calendar,
        Some(pt::wiring::iso_date(Date(19_180)))
    );
    // The run wall-clock + display names thread through. (TUI-VIEW-NAV-013,
    // TUI-VIEW-POS-009)
    assert_eq!(view.updated_hhmm, Some("16:05".to_string()));
    assert_eq!(view.display_names.resolve("AMZN"), "Amazon.com");
    assert_eq!(
        view.display_names.resolve("PLTR"),
        "PLTR",
        "ticker fallback"
    );
    // No integrity failure on a clean projection (derived numbers render).
    assert!(view.integrity.is_none());
    assert!(!view.blocked());
}

// @spec TUI-VIEW-NAV-012
#[test]
fn view_state_threads_no_calendar_for_a_keyless_or_zero_key_cycle() {
    // No priced trading day (no key) → no calendar string: the TUI reads
    // `no priced day yet`, never "as of 0".
    let mut out = outcome();
    out.trading_day_key = None;
    let view = view_state_from_cycle(
        &out,
        BTreeMap::new(),
        BTreeMap::new(),
        Vec::new(),
        Vec::new(),
        None,
        config::BracketState::NoBracketsAvailable,
        Vec::new(),
        2026,
        tui::port::Connection::Live,
        None,
        config::DisplayNameMap::default(),
    );
    assert_eq!(view.as_of_calendar, None);
    assert_eq!(view.updated_hhmm, None);

    // A ZERO key is "no priced day", not 1970-01-01.
    let mut out = outcome();
    out.trading_day_key = Some(TradingDayKey(Date(0)));
    let view = view_state_from_cycle(
        &out,
        BTreeMap::new(),
        BTreeMap::new(),
        Vec::new(),
        Vec::new(),
        None,
        config::BracketState::NoBracketsAvailable,
        Vec::new(),
        2026,
        tui::port::Connection::Live,
        None,
        config::DisplayNameMap::default(),
    );
    assert_eq!(
        view.as_of_calendar, None,
        "a zero key never formats as 1970-01-01"
    );
}

// @spec TUI-VIEW-NAV-013
#[test]
fn hhmm_formats_in_the_reporting_timezone_with_the_us_eastern_dst_rule() {
    use pt::wiring::hhmm_in_reporting_tz;
    // 2026-06-10 18:42:00 UTC → EDT (UTC−4) → 14:42. (summer: DST active)
    // epoch = 20_614 days × 86_400 + 18h42m.
    let summer = 20_614i64 * 86_400 + 18 * 3_600 + 42 * 60;
    assert_eq!(hhmm_in_reporting_tz(summer, "US/Eastern"), "14:42");
    // 2026-01-15 18:42:00 UTC → EST (UTC−5) → 13:42. (winter: standard time)
    // 2026-01-15 = 20_468 days.
    let winter = 20_468i64 * 86_400 + 18 * 3_600 + 42 * 60;
    assert_eq!(hhmm_in_reporting_tz(winter, "US/Eastern"), "13:42");
    // An unrecognized timezone falls back to UTC.
    assert_eq!(hhmm_in_reporting_tz(summer, "Mars/Olympus"), "18:42");
}

#[test]
fn context_for_year_is_regime_agnostic_with_no_brackets() {
    // Until per-year workbook brackets are wired, the binary replays against the
    // regime-agnostic migration context (no brackets => estimates degrade honestly).
    let ctx = context_for_year(2026);
    assert_eq!(ctx.tax_year, config::TaxYear(2026));
    assert!(ctx.federal.ordinary.is_none(), "no fabricated brackets");
}

/// A real-shaped `SeriesPoint` for the history-bundle tests (only the key and
/// total matter to the bundling).
fn point(key: i32, total: i64) -> reports::SeriesPoint {
    reports::SeriesPoint {
        key: TradingDayKey(Date(key)),
        total_market_value_cents: Cents(total),
        total_unrealized_pretax_cents: Cents(0),
        total_unrealized_net_of_tax_cents: Cents(0),
        total_basis_cents: Cents(0),
        per_symbol_value_cents: BTreeMap::new(),
        per_symbol_shares: BTreeMap::new(),
        marks: BTreeMap::new(),
        captured_at_epoch_secs: 0,
        reporting_tz_date: Date(key),
        incomplete: false,
    }
}

// @spec TUI-VIEW-POS-005, TUI-VIEW-HIST-003
#[test]
fn history_bundle_drops_zero_key_garbage_points() {
    use pt::wiring::history_bundle;
    // A leftover key-0 row (a capture taken before any symbol priced — "no
    // priced day") never enters the bundled series: not a chart column, not a
    // day-change prior.
    let (map, calendar) = history_bundle(vec![point(0, 9_999_00), point(20_614, 338_508_46)]);
    assert_eq!(
        calendar,
        vec![TradingDayKey(Date(20_614))],
        "the zero key never enters the calendar"
    );
    assert!(!map.contains_key(&TradingDayKey(Date(0))));
    assert!(map.contains_key(&TradingDayKey(Date(20_614))));

    // Only garbage → an empty series (the no-history states render).
    let (map, calendar) = history_bundle(vec![point(0, 9_999_00)]);
    assert!(map.is_empty() && calendar.is_empty());

    // Last-wins by key survives the filter.
    let (map, _) = history_bundle(vec![point(20_614, 1_00), point(20_614, 2_00)]);
    assert_eq!(
        map[&TradingDayKey(Date(20_614))].total_market_value_cents,
        Cents(2_00)
    );
}

// @spec TUI-VIEW-POS-011
#[test]
fn view_bracket_state_is_configs_resolved_state_for_the_current_year() {
    use pt::wiring::view_bracket_state;
    // The workbook carries ONLY the current year's rules → Verified: the TUI's
    // `[est]` qualifier must reflect the brackets the figures were computed
    // under, never a hardcoded cold-start `n/a (no brackets)`.
    let set = config::BracketSet {
        rows: vec![config::BracketRow {
            lower_threshold_cents: Cents(0),
            rate_ppm: config::Ppm(100_000),
        }],
        last_verified: Date(20_500),
        source_note: "test".to_string(),
    };
    let rules = config::TaxRules {
        tax_year: config::TaxYear(2026),
        filing_status: config::FilingStatus::default(),
        federal_ordinary: set.clone(),
        federal_long_term: set.clone(),
        niit: config::Niit {
            rate_ppm: config::Ppm(38_000),
            magi_threshold_cents: Cents(250_000_00),
        },
        state_ordinary: BTreeMap::new(),
        ordinary_income_cents: Cents(300_000_00),
    };
    let mut data = config::ConfigData::default();
    data.rules_by_year.insert(config::TaxYear(2026), rules);

    assert_eq!(
        view_bracket_state(Some(&data), 2026),
        config::BracketState::Verified,
        "current-year rules → Verified, never n/a (no brackets)"
    );
    // A later year falls back to the most recent prior set, flagged Stale.
    assert_eq!(
        view_bracket_state(Some(&data), 2027),
        config::BracketState::Stale
    );
    // An unreadable workbook config degrades to the honest cold-start.
    assert_eq!(
        view_bracket_state(None, 2026),
        config::BracketState::NoBracketsAvailable
    );
}
