//! Read-only screen tests (TUI-VIEW-POS / LOT / HIST / TAX / REAL).

mod common;

use pt_core::{Cents, Date};
use reports::TradingDayKey;
use tui::testkit::{
    buy, flat_federal_ctx, render_string, replay, series_point, FakeRuntime, ViewBuilder,
};
use tui::views::{self, Filter, Grouping, NavState, Screen};
use tui::{screen_lines, Model};

// @spec TUI-VIEW-POS-001
#[test]
fn positions_render_per_symbol_value_net_est_delta_share_and_sparkline() {
    let view = common::two_position_view();
    let nav = NavState::new(Screen::Positions);
    let (rows, _caveat, _hdr) = views::positions_rows(&view, &nav);
    assert_eq!(rows.len(), 2, "two priced symbols");
    let amzn = rows.iter().find(|r| r.label == "AMZN").unwrap();
    assert!(amzn.market_value.is_some(), "priced market value");
    assert!(amzn.net_post_tax.is_some(), "post-tax net [est]");
    assert!(amzn.share_ppm.is_some(), "share % of the priced portfolio");
    assert!(!amzn.degraded);

    // The rendered line carries the [est] qualifier on the net + the share %, with
    // the qualifier segment in the estimate role (the spans seam).
    let lines = screen_lines(&view, &nav);
    let amzn_line = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();
    assert!(
        amzn_line.text().contains("[est]"),
        "the post-tax net carries [est]: {}",
        amzn_line.text()
    );
    assert_eq!(
        amzn_line.span_role("[est]"),
        Some(tui::theme::Role::Estimate),
        "the [est] qualifier renders in the estimate role"
    );
}

// @spec TUI-VIEW-POS-002
#[test]
fn grouping_toggle_repivots_by_platform_with_full_group_subtotals_under_filter() {
    // Two symbols on Robinhood + a third on Schwab.
    let log = vec![
        buy(1, 18_000, "a", "AMZN", 10, 5_000, "Robinhood"),
        buy(2, 18_100, "g", "GOOGL", 10, 5_000, "Robinhood"),
        buy(3, 18_200, "m", "MSFT", 10, 5_000, "Schwab"),
    ];
    let snap = replay(&log, &[("AMZN", 6_000), ("GOOGL", 6_000), ("MSFT", 6_000)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 6_000, Date(20_000))
        .mark("GOOGL", 6_000, Date(20_000))
        .mark("MSFT", 6_000, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .estimate("GOOGL", common::est("GOOGL", 10_00, 2_00))
        .estimate("MSFT", common::est("MSFT", 10_00, 2_00))
        .build();

    let mut nav = NavState::new(Screen::Positions);
    nav.grouping = Grouping::ByPlatform;
    let (rows, _c, _h) = views::positions_rows(&view, &nav);
    assert!(
        rows.iter().any(|r| r.label == "Robinhood"),
        "re-pivoted by platform"
    );
    assert!(rows.iter().any(|r| r.label == "Schwab"));

    // The group subtotal reflects the FULL group: the Robinhood subtotal is the sum
    // of both its symbols. 10 sh @ $60 × 2 symbols = $1,200.00 = 120000 cents.
    let robinhood = rows.iter().find(|r| r.label == "Robinhood").unwrap();
    assert_eq!(
        robinhood.market_value,
        Some(Cents(120_000)),
        "the subtotal reflects the full group"
    );

    // Under an ACTIVE filter while grouped by platform, the subtotal STAYS WHOLE: a
    // symbol filter is inapplicable to the platform pivot (it does not garbage-match
    // the symbol against the platform label), so the Robinhood group stays visible
    // with its full $1,200 subtotal — never rebased to the filtered subset.
    // (TUI-VIEW-POS-002 — the load-bearing "even under an active filter" clause)
    nav.filter = Filter::Symbol("AMZN".to_string());
    let (filtered_rows, _c, _h) = views::positions_rows(&view, &nav);
    let robinhood_filtered = filtered_rows
        .iter()
        .find(|r| r.label == "Robinhood")
        .expect("the platform group stays visible under a symbol filter — subtotal whole");
    assert_eq!(
        robinhood_filtered.market_value,
        Some(Cents(120_000)),
        "the group subtotal is unchanged under an active filter (stays whole, not rebased)"
    );

    // A PLATFORM filter under ByPlatform narrows which groups show (the matching
    // group's subtotal still whole); a non-matching platform is hidden.
    nav.filter = Filter::Platform("Robinhood".to_string());
    let (plat_rows, _c, _h) = views::positions_rows(&view, &nav);
    assert!(plat_rows.iter().any(|r| r.label == "Robinhood"));
    assert!(
        !plat_rows.iter().any(|r| r.label == "Schwab"),
        "a platform filter narrows the visible groups"
    );
    assert_eq!(
        plat_rows
            .iter()
            .find(|r| r.label == "Robinhood")
            .unwrap()
            .market_value,
        Some(Cents(120_000)),
        "the surviving group's subtotal is still whole"
    );
}

// @spec TUI-VIEW-POS-003
#[test]
fn priced_coverage_caveat_on_header_and_share_na_distinct_from_degraded() {
    // A degraded (unpriced) symbol → the caveat counts it + the share column for
    // the priced rows is well-defined; an all-unpriced book → shares n/a.
    let log = vec![
        buy(1, 18_000, "a", "AMZN", 10, 5_000, "Robinhood"),
        buy(2, 18_100, "p", "PLTR", 10, 5_000, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 6_000)]); // PLTR unpriced
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 6_000, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .degraded("PLTR", sheets_view::DegradeReason::Permanent)
        .build();
    let comp = views::composition(&view);
    let caveat = views::composition_caveat(&comp);
    assert_eq!(
        caveat.degraded_count, 1,
        "PLTR counted in the priced-coverage caveat"
    );
    assert!(
        !caveat.shares_na,
        "AMZN is priced → shares are well-defined (not n/a)"
    );

    let nav = NavState::new(Screen::Positions);
    let (rows, _c, _h) = views::positions_rows(&view, &nav);
    let pltr = rows.iter().find(|r| r.label == "PLTR").unwrap();
    assert!(pltr.degraded, "PLTR is degraded (the ‡ marker, never 0)");
    assert!(
        !pltr.share_na,
        "the degraded ‡ marker is distinct from share n/a"
    );

    // An ALL-unpriced book → the share column is n/a (in reports' wording).
    let snap2 = replay(&replay_log_all_unpriced(), &[]);
    let view2 = ViewBuilder::new(snap2).build();
    let caveat2 = views::composition_caveat(&views::composition(&view2));
    assert!(caveat2.shares_na, "an all-unpriced book → shares n/a");
}

fn replay_log_all_unpriced() -> Vec<ledger_core::LedgerEvent> {
    vec![buy(1, 18_000, "a", "AMZN", 10, 5_000, "Robinhood")]
}

// @spec TUI-VIEW-LOT-001
#[test]
fn open_lots_render_per_lot_fields_and_are_the_drill_down_target() {
    let log = vec![
        buy(1, 15_000, "L-A", "AMZN", 100, 800, "Robinhood"),
        buy(2, 19_000, "L-B", "GOOGL", 60, 10_000, "Schwab"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126), ("GOOGL", 37_637)]);
    let view = ViewBuilder::new(snap).build();
    let nav = NavState::new(Screen::OpenLots);
    let (rows, _hdr) = views::lot_rows(&view, &nav);
    assert_eq!(rows.len(), 2);
    let a = rows.iter().find(|r| r.lot_id == "L-A").unwrap();
    assert_eq!(a.symbol, "AMZN");
    assert_eq!(a.acquire_date, Date(15_000));
    assert_eq!(a.source, ledger_core::LotSource::Buy);
    assert_eq!(a.remaining_qty, common::sh(100));
    assert_eq!(a.basis, Cents(80_000)); // 100 sh @ $8
    assert_eq!(a.basis_per_share, Cents(800));
    assert_eq!(a.platform, "Robinhood");

    // Open Lots is the drill target from a Position.
    assert_eq!(
        views::drill_target(
            &Screen::Positions,
            &views::RowIdentity::Symbol("AMZN".to_string())
        ),
        Some((Screen::OpenLots, views::Scope::Symbol("AMZN".to_string())))
    );
}

// @spec TUI-VIEW-HIST-001
#[test]
fn history_renders_chart_plus_table_with_gap_blank_and_incomplete_tinted() {
    // A 3-day calendar: day A captured, day B a GAP (no capture), day C incomplete.
    let snap = ledger_core::Snapshot::default();
    let view = ViewBuilder::new(snap)
        .history_point(series_point(
            TradingDayKey(Date(100)),
            1_000_00,
            &[("AMZN", 1_000_00)],
            false,
        ))
        .calendar_day(TradingDayKey(Date(101))) // a gap
        .history_point(series_point(
            TradingDayKey(Date(102)),
            1_200_00,
            &[("AMZN", 1_200_00)],
            true,
        ))
        .build();
    let nav = NavState::new(Screen::History);
    let hv = views::history_view(&view, &nav);
    assert!(matches!(hv.state, views::HistoryState::Normal));
    assert!(
        matches!(hv.cells[0], views::HistoryCell::Value { .. }),
        "day A is a value"
    );
    assert!(
        matches!(hv.cells[1], views::HistoryCell::Gap { .. }),
        "day B is an explicit gap (blank, not interpolated)"
    );
    assert!(
        matches!(hv.cells[2], views::HistoryCell::Incomplete { .. }),
        "day C is a ‡-tinted incomplete cell"
    );

    // The rendered gap line shows the gap convention; the incomplete shows ‡.
    let lines = screen_lines(&view, &nav);
    assert!(
        lines.iter().any(|l| l.text().contains("gap")),
        "an in-band gap renders blank-ish"
    );
    assert!(
        lines.iter().any(|l| l.text().contains('\u{2021}')),
        "an incomplete day carries ‡"
    );
}

// @spec TUI-VIEW-HIST-002
#[test]
fn history_degenerate_states_render_explicitly() {
    let snap = ledger_core::Snapshot::default();
    let nav = NavState::new(Screen::History);

    // 0 points → no-history.
    let v0 = ViewBuilder::new(snap.clone()).build();
    assert!(matches!(
        views::history_view(&v0, &nav).state,
        views::HistoryState::NoHistory
    ));

    // 1 point → a single tick (no trend).
    let v1 = ViewBuilder::new(snap.clone())
        .history_point(series_point(TradingDayKey(Date(100)), 1_000_00, &[], false))
        .build();
    assert!(matches!(
        views::history_view(&v1, &nav).state,
        views::HistoryState::SinglePoint
    ));

    // All-incomplete → the chart is suppressed, the table flagged.
    let vi = ViewBuilder::new(snap)
        .history_point(series_point(TradingDayKey(Date(100)), 1_000_00, &[], true))
        .history_point(series_point(TradingDayKey(Date(101)), 1_100_00, &[], true))
        .build();
    let hv = views::history_view(&vi, &nav);
    assert!(matches!(hv.state, views::HistoryState::AllIncomplete));
    assert!(
        hv.sparkline.is_empty(),
        "the chart is suppressed when all-incomplete"
    );
}

// @spec TUI-VIEW-TAX-001
#[test]
fn tax_renders_accruals_with_stepper_grouped_with_reserves_and_provenance() {
    let acc = tui::testkit::accrual_moved("S1", "L1");
    let view = ViewBuilder::new(ledger_core::Snapshot::default())
        .accruals(vec![acc])
        .annual_rows(vec![tax::AnnualRow {
            jurisdiction: config::Jurisdiction::Federal,
            tax_year: config::TaxYear(common::YEAR),
            accrued_cents: Cents(240_000),
            moved_cents: Cents(240_000),
            paid_cents: Cents(0),
            outstanding_cents: Cents(240_000),
            shortfall_cents: Cents(0),
            gain_cents: Cents(10_000_00),
            effective_rate_ppm: Some(config::Ppm(200_000)),
            bracket_state: config::BracketState::Verified,
        }])
        .next_payment_period(tax::Quarter::Q2)
        .build();
    let nav = NavState::new(Screen::TaxReserves);
    let (rows, reserves, _hdr) = views::tax_rows(&view, &nav);
    assert_eq!(rows.len(), 1);
    // The Moved stepper renders ◉◉◉○.
    assert!(matches!(
        rows[0].stepper,
        tui::theme::StepperState::Lifecycle(tui::theme::LifecycleStop::Moved)
    ));
    let step = tui::theme::stepper(&rows[0].stepper);
    assert!(step.contains("Moved"));
    assert_eq!(reserves.len(), 1);
    assert_eq!(reserves[0].outstanding, Cents(240_000));

    // The provenance caveat distinguishes estimated tax from the gains themselves.
    let lines = screen_lines(&view, &nav);
    assert!(lines.iter().any(|l| l.text() == views::TAX_PROVENANCE));
    assert!(views::TAX_PROVENANCE.contains("not the gains themselves"));

    // The next estimated-payment period is rendered (from tax's quarterly report).
    assert!(
        lines
            .iter()
            .any(|l| l.text().contains("next estimated payment") && l.text().contains("Q2")),
        "the next estimated-payment period appears on the Tax & Reserves screen"
    );
}

// @spec TUI-VIEW-TAX-001
#[test]
fn tax_renders_autosettled_and_orphan_states_distinctly() {
    // A de-minimis accrual → ✓ settled (NOT a partial stepper), excluded from
    // actionable selection. An orphan → ⚠ undone. (tui-design / TAX-VERIF-007)
    let mut settled = tui::testkit::accrual_moved("Sde", "Lde");
    settled.de_minimis = true;
    let orphaned = tui::testkit::accrual_moved("Sorph", "Lorph");
    let orphan_key = orphaned.key.clone();

    let view = ViewBuilder::new(ledger_core::Snapshot::default())
        .accruals(vec![settled, orphaned])
        .orphans(vec![tax::OrphanWarning {
            accrual_key: orphan_key.clone(),
            kind: tax::OrphanKind::Move,
        }])
        .build();
    let nav = NavState::new(Screen::TaxReserves);
    let (rows, _r, _h) = views::tax_rows(&view, &nav);

    let settled_row = rows.iter().find(|r| r.key.sale_id == "Sde").unwrap();
    assert!(matches!(
        settled_row.stepper,
        tui::theme::StepperState::AutoSettled
    ));
    assert!(
        !settled_row.actionable,
        "a de-minimis auto-settled accrual is excluded from actionable selection"
    );
    assert!(tui::theme::stepper(&settled_row.stepper).contains("settled"));

    let orphan_row = rows.iter().find(|r| &r.key == &orphan_key).unwrap();
    assert!(matches!(
        orphan_row.stepper,
        tui::theme::StepperState::Orphaned
    ));
    assert!(tui::theme::stepper(&orphan_row.stepper).contains("undone"));
}

// @spec TUI-VIEW-TAX-002
#[test]
fn tax_launches_entry_flow_passing_identity_for_live_reresolve() {
    // Selecting an accrual for action launches the matching entry flow passing the
    // accrual's IDENTITY; entry re-resolves it live (never the rendered values).
    let key = tax::AccrualKey {
        sale_id: "S1".to_string(),
        lot_id: "L1".to_string(),
        jurisdiction: config::Jurisdiction::Federal,
        tax_year: config::TaxYear(common::YEAR),
    };
    let identity = tui::entry::LaunchIdentity::Accrual(key.clone());
    assert!(matches!(identity, tui::entry::LaunchIdentity::Accrual(_)));

    // entry re-resolves the identity against the CURRENT accruals (the live values).
    let live = vec![tui::testkit::accrual_moved("S1", "L1")];
    let resolved = tui::entry::reresolve_accrual(&key, &live).expect("re-resolves live");
    assert_eq!(resolved.key, key);
    // A vanished accrual (reversed sale) re-resolves to None — never a stale render.
    assert!(tui::entry::reresolve_accrual(&key, &[]).is_none());
}

// @spec TUI-VIEW-REAL-001
#[test]
fn realized_renders_calendar_years_labelled_calendar_with_provenance() {
    // A Buy then a Sell in 2025 → a calendar-2025 realized row.
    let log = vec![
        buy(1, 18_000, "L-A", "AMZN", 100, 5_000, "Robinhood"),
        ledger_core::LedgerEvent {
            id: "e2".to_string(),
            seq: pt_core::Seq(2),
            date: Date(20_100), // 2025-ish
            kind: ledger_core::LedgerEventKind::Sell {
                sale_id: "S".to_string(),
                symbol: "AMZN".to_string(),
                qty: common::sh(100),
                unit_price_cents: Cents(9_989),
                fees_cents: Cents(0),
                lot_refs: vec![ledger_core::LotRef {
                    lot_id: "L-A".to_string(),
                    qty: common::sh(100),
                }],
                accrues_to_state: None,
                platform: "Robinhood".to_string(),
                tracking_code: None,
            },
        },
    ];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = ViewBuilder::new(snap).build();
    let nav = NavState::new(Screen::Realized);
    let (rows, _hdr) = views::realized_rows(&view, &nav);
    assert_eq!(rows.len(), 1, "one calendar year of realized gains");
    assert!(
        rows[0].label.starts_with("calendar "),
        "labelled calendar (distinct from tax's quarterly)"
    );

    // The provenance caveat marks these as raw realized gains, not tax.
    let lines = screen_lines(&view, &nav);
    assert!(lines.iter().any(|l| l.text() == views::REALIZED_PROVENANCE));
    assert!(views::REALIZED_PROVENANCE.contains("not tax"));
}

// @spec TUI-VIEW-POS-001
#[test]
fn rendered_buffer_carries_the_masthead_and_status_line() {
    // The rendered buffer (no TTY) carries the masthead + a status line.
    let rt: FakeRuntime = FakeRuntime::new(
        common::two_position_view(),
        flat_federal_ctx(common::YEAR, 220_000),
    );
    let model = Model::new();
    let s = render_string(&model, &rt, 90, 20);
    assert!(s.contains("L E D G E R"), "the masthead renders");
    assert!(
        s.contains("connected") || s.contains("stale"),
        "the status line renders the connection"
    );
}

// ===========================================================================
// Motif pins from the divergence audit: the screens render the Ledger language
// the designs describe — caret-only-on-focus, full POS/LOT columns, the summary
// band + double-entry rule, the header-band as-of, ramp-coloured steppers,
// direction-tinted sparklines, human screen titles, and the six empty states.
// ===========================================================================

// @spec TUI-VIEW-POS-001
#[test]
fn positions_row_carries_shares_price_and_pretax_unrealized_columns() {
    // POS-001 lists shares · price · market value · pre-tax unrealized · net [est]
    // · Δ · sparkline · share % — ALL on the row.
    let view = common::two_position_view();
    let nav = NavState::new(Screen::Positions);
    let (rows, _c, _h) = views::positions_rows(&view, &nav);
    let amzn = rows.iter().find(|r| r.label == "AMZN").unwrap();
    assert_eq!(
        amzn.shares,
        Some(common::sh(620)),
        "the share count rides the row"
    );
    assert_eq!(
        amzn.price,
        Some(Cents(26_126)),
        "the mark price rides the row"
    );
    assert!(amzn.unrealized_pretax.is_some());

    let lines = screen_lines(&view, &nav);
    let amzn = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();
    let amzn_line = amzn.text();
    assert!(amzn_line.contains("620"), "shares rendered: {amzn_line}");
    assert!(amzn_line.contains("$261.26"), "price rendered: {amzn_line}");
    // The pre-tax unrealized renders with its redundant sign+glyph (620 sh bought
    // at $50, marked $261.26 → a ▲ gain column distinct from the net [est]) — and
    // the spans seam carries the gain role on that segment alone.
    assert!(
        amzn_line.contains('\u{25B2}'),
        "pre-tax unrealized carries ▲: {amzn_line}"
    );
    assert_eq!(
        amzn.span_role("\u{25B2}"),
        Some(tui::theme::Role::Gain),
        "the unrealized delta segment carries the gain role"
    );
    // Decimal-aligned ledger columns: the dim │ column rules separate the figures.
    assert!(
        amzn_line.matches('│').count() >= 5,
        "│-ruled columns: {amzn_line}"
    );
    assert_eq!(
        amzn.span_role("│"),
        Some(tui::theme::Role::FgFaint),
        "the column rules stay dim chrome"
    );
}

// @spec TUI-VIEW-POS-001, TUI-VIEW-POS-010
#[test]
fn positions_render_the_total_value_and_net_post_tax_summary_band_with_double_rule() {
    // The landing screen's live glance: TOTAL VALUE + NET (POST-TAX) [est] in
    // tracked caps over a ═ double-entry rule (tui-design → "A screen in this
    // language" / "Tracked-caps labels" / "Panels & rules"). Totals come from
    // reports' series-point builder — views computes nothing.
    let view = common::two_position_view();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);
    let total = lines
        .iter()
        .find(|l| l.text().starts_with("TOTAL VALUE"))
        .expect("TOTAL VALUE band");
    // 620 sh @ $261.26 = $161,981.20 + 60 sh @ $376.37 = $22,582.20 → $184,563.40,
    // rendered as the band's exact whole dollars (reconciliation money — never
    // abbreviated, never with cents). (TUI-VIEW-POS-010)
    assert!(
        total.text().contains("$184,563"),
        "the whole-book total: {}",
        total.text()
    );
    assert!(
        !total.text().contains("$184.6k"),
        "the band never abbreviates: {}",
        total.text()
    );
    // The type hierarchy rides the spans: the tracked-caps label is faint, the
    // figure beside it bold fg. (tui-design → "Tracked-caps labels")
    assert_eq!(
        total.span_role("TOTAL VALUE"),
        Some(tui::theme::Role::FgFaint)
    );
    let figure = total
        .spans
        .iter()
        .find(|s| s.text.contains("$184,563"))
        .unwrap();
    assert_eq!(figure.role, tui::theme::Role::Fg);
    assert!(
        figure.bold,
        "the figure beside a tracked-caps label is bold"
    );
    let net = lines
        .iter()
        .find(|l| l.text().starts_with("NET (POST-TAX)"))
        .expect("NET band");
    assert!(
        net.text().contains("[est]"),
        "the post-tax net is an estimate: {}",
        net.text()
    );
    assert_eq!(
        net.span_role("[est]"),
        Some(tui::theme::Role::Estimate),
        "the [est] qualifier renders in the estimate role"
    );
    // The double-entry rule separates the band from the rows.
    assert!(
        lines.iter().any(|l| l.text().starts_with('\u{2550}')),
        "a ═ rule renders"
    );
    // The band precedes the rows.
    let band_idx = lines
        .iter()
        .position(|l| l.text().starts_with("TOTAL VALUE"))
        .unwrap();
    let row_idx = lines
        .iter()
        .position(|l| l.text().contains("AMZN"))
        .unwrap();
    assert!(band_idx < row_idx, "the glance band sits above the rows");
}

// @spec TUI-VIEW-POS-003
#[test]
fn positions_summary_band_never_renders_zero_for_an_unpriced_book() {
    // An all-unpriced book: the total is dash + ‡ unpriced — never a fabricated $0.
    let log = vec![buy(1, 18_000, "a", "AMZN", 10, 5_000, "Robinhood")];
    let snap = replay(&log, &[]);
    let view = ViewBuilder::new(snap)
        .degraded("AMZN", sheets_view::DegradeReason::Permanent)
        .build();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);
    let total = lines
        .iter()
        .find(|l| l.text().starts_with("TOTAL VALUE"))
        .expect("band renders");
    assert!(
        !total.text().contains("$0"),
        "never a fabricated zero: {}",
        total.text()
    );
    assert!(
        total.text().contains('\u{2021}'),
        "the ‡ marker travels with the figure: {}",
        total.text()
    );
}

// @spec TUI-VIEW-NAV-004
#[test]
fn focus_caret_marks_only_the_focused_row_over_the_bg_focus_token() {
    // The ▎ caret is the focused row's left gutter — never on every row (tui-design
    // → "Focus caret"), and the focused row renders over bg-focus.
    let view = common::two_position_view();
    let mut nav = NavState::new(Screen::Positions);
    nav.focus = views::RowIdentity::Symbol("GOOGL".to_string());
    let lines = screen_lines(&view, &nav);
    let amzn = lines
        .iter()
        .find(|l| l.text().contains("AMZN"))
        .unwrap()
        .text();
    let googl = lines
        .iter()
        .find(|l| l.text().contains("GOOGL"))
        .unwrap()
        .text();
    assert!(
        googl.starts_with('\u{258E}'),
        "the focused row carries the ▎ caret: {googl}"
    );
    assert!(
        !amzn.starts_with('\u{258E}'),
        "an unfocused row has a blank gutter: {amzn}"
    );

    // The bg-focus chrome token backs the caret row in the rendered buffer.
    use tui::theme::{ColorDepth, Palette};
    assert_eq!(
        Palette::at(ColorDepth::TrueColor).focus_bg(),
        Some(ratatui::style::Color::Rgb(0x2A, 0x26, 0x18)),
        "bg-focus is the Ledger #2A2618"
    );
    let rt = FakeRuntime::new(view, flat_federal_ctx(common::YEAR, 220_000));
    let mut model = Model::new();
    model.current_mut().nav.focus = views::RowIdentity::Symbol("GOOGL".to_string());
    let buf = tui::render_to_buffer(&model, &rt, 100, 24);
    let mut found = false;
    for y in 0..24u16 {
        if buf[(1, y)].symbol() == "\u{258E}" {
            assert_eq!(
                buf[(1, y)].style().bg,
                Some(ratatui::style::Color::Rgb(0x2A, 0x26, 0x18)),
                "the caret row renders over bg-focus"
            );
            found = true;
        }
    }
    assert!(found, "the focused row rendered with its caret");
}

// @spec TUI-VIEW-POS-001, TUI-VIEW-NAV-012
#[test]
fn header_band_carries_the_right_aligned_as_of_and_stale_glyph_when_offline() {
    // The header band: accent title left, right-aligned as-of — always the same
    // spot, as a CALENDAR date (the formatted string the binary threads), never a
    // raw trading-day key int; offline marks it ⟲ stale, never presented as live.
    // (tui-design → "Header band" / "Freshness"; TUI-VIEW-NAV-012)
    let rt = FakeRuntime::new(
        common::two_position_view(),
        flat_federal_ctx(common::YEAR, 220_000),
    );
    let model = Model::new();
    let s = render_string(&model, &rt, 100, 20);
    let first = s.lines().next().unwrap();
    assert!(first.contains("L E D G E R"), "masthead left");
    assert!(
        first.trim_end().ends_with("as-of 2024-10-04"),
        "right-aligned calendar as-of: {first}"
    );
    assert!(
        !first.contains("as-of 20000"),
        "never the raw key int: {first}"
    );
    assert!(!first.contains('\u{27F2}'), "live is not stale-marked");

    let mut view = common::two_position_view();
    view.connection = tui::port::Connection::Offline;
    let rt2 = FakeRuntime::new(view, flat_federal_ctx(common::YEAR, 220_000));
    let s2 = render_string(&model, &rt2, 100, 20);
    let first2 = s2.lines().next().unwrap();
    assert!(
        first2.contains('\u{27F2}'),
        "offline carries the ⟲ stale glyph: {first2}"
    );
    assert!(
        first2.contains("as-of 2024-10-04"),
        "with the last calendar as-of: {first2}"
    );
}

// @spec TUI-VIEW-NAV-012
#[test]
fn a_book_with_no_priced_day_reads_no_priced_day_yet_never_zero() {
    use tui::testkit::ViewBuilder;
    // No priced trading day → the masthead and the status line read the explicit
    // wording, never "as of 0" / a raw key int.
    let view = ViewBuilder::new(ledger_core::Snapshot::default())
        .no_priced_day()
        .build();
    let rt = FakeRuntime::new(view.clone(), flat_federal_ctx(common::YEAR, 220_000));
    let model = Model::new();
    let s = render_string(&model, &rt, 100, 20);
    let first = s.lines().next().unwrap();
    assert!(
        first.contains("no priced day yet"),
        "the masthead names the state: {first}"
    );
    assert!(
        !first.contains("as-of 0") && !first.contains("as-of 20000"),
        "never a key int: {first}"
    );

    let status = tui::StatusLine::from_view(&view, tui::Mode::Views, &Screen::Positions);
    assert!(
        status.text().contains("no priced day yet"),
        "the status line too: {}",
        status.text()
    );
}

// @spec TUI-VIEW-NAV-013
#[test]
fn status_line_carries_the_updated_wall_clock_and_omits_it_when_unknown() {
    use tui::testkit::ViewBuilder;
    // The `updated HH:MM` wall-clock (threaded by the binary) rides the status
    // line; a view with no run time omits the segment rather than fabricating one.
    let view = ViewBuilder::new(ledger_core::Snapshot::default())
        .updated("16:42")
        .build();
    let status = tui::StatusLine::from_view(&view, tui::Mode::Views, &Screen::Positions);
    assert!(
        status.text().contains("updated 16:42"),
        "the wall-clock renders: {}",
        status.text()
    );

    let bare = ViewBuilder::new(ledger_core::Snapshot::default()).build();
    let status = tui::StatusLine::from_view(&bare, tui::Mode::Views, &Screen::Positions);
    assert!(
        !status.text().contains("updated"),
        "no fabricated time: {}",
        status.text()
    );

    // And it renders in the buffer's bottom line.
    let rt = FakeRuntime::new(view, flat_federal_ctx(common::YEAR, 220_000));
    let model = Model::new();
    let s = render_string(&model, &rt, 120, 20);
    let bottom = s.lines().last().unwrap();
    assert!(
        bottom.contains("updated 16:42"),
        "the status line renders it: {bottom}"
    );
}

// @spec TUI-VIEW-LOT-001, TUI-VIEW-LOT-004
#[test]
fn open_lots_line_renders_acquire_date_basis_and_tracking_code() {
    // LOT-001 lists id · symbol · acquire date · source · term · remaining qty ·
    // basis · basis/share · platform · tracking code — ALL on the line.
    let log = vec![buy(1, 15_000, "L-A", "AMZN", 100, 800, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = ViewBuilder::new(snap).build();
    let nav = NavState::new(Screen::OpenLots);
    let lines = screen_lines(&view, &nav);
    let line = lines
        .iter()
        .find(|l| l.text().contains("L-A"))
        .unwrap()
        .text();
    assert!(line.contains("15000"), "acquire date rendered: {line}");
    // BASIS is glance money (compact whole dollars); $/SH is per-share money
    // and keeps its cents. (TUI-VIEW-LOT-004)
    assert!(
        line.contains("basis $800"),
        "the lot basis rendered (100 sh @ $8): {line}"
    );
    assert!(
        !line.contains("$800.00"),
        "the basis drops its cents: {line}"
    );
    assert!(line.contains("$8.00/sh"), "basis/share keeps cents: {line}");
    assert!(
        line.contains('\u{2014}'),
        "an absent tracking code renders the — dash: {line}"
    );
    assert!(line.contains("Robinhood"));
}

// @spec TUI-VIEW-TAX-001
#[test]
fn tax_rows_group_by_jurisdiction_year_and_colour_by_the_minting_ramp() {
    // Accruals grouped by (jurisdiction, tax_year) under a tracked-caps group
    // header; each row coloured by the ramp — Accrued faint → Allocated verdigris
    // → Moved gilt → Paid sage; settled dim; orphan warn. The stepper dots + word
    // stay the redundant signal. (tui-design → "Color Conventions")
    let mut accrued = tui::testkit::accrual_moved("S1", "L1");
    accrued.state = tax::AccrualState::Accrued;
    let mut alloc = tui::testkit::accrual_moved("S2", "L2");
    alloc.state = tax::AccrualState::Allocated {
        account_label: "Reserve".into(),
    };
    let moved = tui::testkit::accrual_moved("S3", "L3");
    let mut paid = tui::testkit::accrual_moved("S4", "L4");
    paid.state = tax::AccrualState::Paid {
        amount_cents: Cents(240_000),
        moved_cents: Cents(240_000),
        date: pt_core::Date(20_120),
        period: tax::Quarter::Q2,
    };
    let view = ViewBuilder::new(ledger_core::Snapshot::default())
        .accruals(vec![accrued, alloc, moved, paid])
        .build();
    let nav = NavState::new(Screen::TaxReserves);
    let lines = screen_lines(&view, &nav);

    // The group header opens the (jurisdiction, tax_year) group.
    assert!(
        lines.iter().any(|l| l.text().starts_with("FEDERAL 2026")
            && l.span_role("FEDERAL") == Some(tui::theme::Role::FgFaint)),
        "a tracked-caps (jurisdiction, tax_year) group header renders"
    );
    // On the spans seam the stepper DOTS carry the ramp colour while the row text
    // (key, amount) stays fg. (views-design → "Line Rendering")
    let row_of = |needle: &str| lines.iter().find(|l| l.text().contains(needle)).unwrap();
    let dots_role = |needle: &str| row_of(needle).span_role("\u{25C9}").unwrap();
    assert_eq!(
        dots_role("S1/L1"),
        tui::theme::Role::FgFaint,
        "Accrued dots render faint"
    );
    assert_eq!(
        dots_role("S2/L2"),
        tui::theme::Role::Estimate,
        "Allocated dots render verdigris"
    );
    assert_eq!(
        dots_role("S3/L3"),
        tui::theme::Role::Accent,
        "Moved dots render gilt"
    );
    assert_eq!(
        dots_role("S4/L4"),
        tui::theme::Role::Gain,
        "Paid dots render sage"
    );
    assert_eq!(
        row_of("S3/L3").span_role("S3/L3"),
        Some(tui::theme::Role::Fg),
        "the row text stays fg while the dots carry the ramp"
    );
}

// @spec TUI-VIEW-HIST-001
#[test]
fn history_chart_is_tinted_by_net_direction_not_chrome() {
    // The History chart is a data motif: tinted gain/loss by net direction
    // (tui-design → "Sparklines"), never the chrome accent. The today tick (the
    // latest captured column) rides bold in the same tint.
    let is_ramp = |s: &&tui::Seg| s.text.chars().any(|c| tui::theme::SPARK_RAMP.contains(&c));
    let bottom_row = |lines: &[tui::ScreenLine]| {
        lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| is_ramp(&s)))
            .next_back()
            .expect("a chart row")
            .clone()
    };

    let snap = ledger_core::Snapshot::default();
    let rising = ViewBuilder::new(snap.clone())
        .history_point(series_point(TradingDayKey(Date(100)), 1_000_00, &[], false))
        .history_point(series_point(TradingDayKey(Date(101)), 1_500_00, &[], false))
        .build();
    let nav = NavState::new(Screen::History);
    let row = bottom_row(&screen_lines(&rising, &nav));
    let first = row.spans.iter().find(is_ramp).unwrap();
    assert_eq!(
        first.role,
        tui::theme::Role::Gain,
        "a rising series tints gain"
    );
    // The latest captured column is brightened — the "today" tick, bold, in the
    // same direction tint. (tui-design → "Sparklines"; TUI-VIEW-HIST-003)
    let tick = row.spans.iter().rev().find(|s| is_ramp(s)).unwrap();
    assert!(
        tick.bold,
        "the latest captured column is brightened (the today tick)"
    );
    assert_eq!(
        tick.role,
        tui::theme::Role::Gain,
        "the tick keeps the direction tint"
    );

    let falling = ViewBuilder::new(snap)
        .history_point(series_point(TradingDayKey(Date(100)), 1_500_00, &[], false))
        .history_point(series_point(TradingDayKey(Date(101)), 1_000_00, &[], false))
        .build();
    let row = bottom_row(&screen_lines(&falling, &nav));
    let first = row.spans.iter().find(is_ramp).unwrap();
    assert_eq!(
        first.role,
        tui::theme::Role::Loss,
        "a falling series tints loss"
    );
    assert!(
        row.spans.iter().rev().find(|s| is_ramp(s)).unwrap().bold,
        "the today tick brightens on a falling series too"
    );
}

// @spec TUI-VIEW-NAV-006
#[test]
fn realized_and_open_lots_render_their_dedicated_empty_states() {
    // The Realized screen renders the dedicated no-realized-gains state (never the
    // "no positions" wording); Open Lots with a non-empty book renders the
    // all-lots-sold state. (TUI-VIEW-NAV-006)
    let log = vec![
        buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood"),
        ledger_core::LedgerEvent {
            id: "e2".to_string(),
            seq: pt_core::Seq(2),
            date: Date(20_100),
            kind: ledger_core::LedgerEventKind::Sell {
                sale_id: "S".to_string(),
                symbol: "AMZN".to_string(),
                qty: common::sh(10),
                unit_price_cents: Cents(9_989),
                fees_cents: Cents(0),
                lot_refs: vec![ledger_core::LotRef {
                    lot_id: "L-A".to_string(),
                    qty: common::sh(10),
                }],
                accrues_to_state: None,
                platform: "Robinhood".to_string(),
                tracking_code: None,
            },
        },
    ];
    let snap = replay(&log, &[("AMZN", 26_126)]); // every lot fully sold
    let sold_out = ViewBuilder::new(snap).build();
    let lines = screen_lines(&sold_out, &NavState::new(Screen::OpenLots));
    assert!(
        lines
            .iter()
            .any(|l| l.text() == "no open lots — every lot has been fully sold"),
        "the all-lots-sold state renders: {lines:?}"
    );

    // A book with positions but nothing sold: Realized renders its own state.
    let log2 = vec![buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood")];
    let unsold = ViewBuilder::new(replay(&log2, &[("AMZN", 26_126)])).build();
    let lines = screen_lines(&unsold, &NavState::new(Screen::Realized));
    assert!(
        lines
            .iter()
            .any(|l| l.text() == "no realized gains yet — they appear when you sell a lot"),
        "the no-realized-gains state renders: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.text().contains("no positions yet")),
        "Realized never reuses the no-positions wording"
    );
}

// @spec TUI-VIEW-NAV-001
#[test]
fn screens_use_their_design_names_and_only_positions_shows_the_of_book_segment() {
    // Panel titles + the status line use the design's screen names ("Open Lots",
    // "Tax & Reserves"), never the Debug identifiers; the `of book` header segment
    // explains the share column, so it renders on Positions only.
    assert_eq!(Screen::OpenLots.title(), "Open Lots");
    assert_eq!(Screen::TaxReserves.title(), "Tax & Reserves");

    let view = common::two_position_view();
    let rt = FakeRuntime::new(view.clone(), flat_federal_ctx(common::YEAR, 220_000));
    let mut model = Model::new();
    model.stack = vec![tui::Frame::new(Screen::TaxReserves)];
    let s = render_string(&model, &rt, 100, 20);
    assert!(s.contains("Tax & Reserves"), "the design name renders: {s}");
    assert!(!s.contains("TaxReserves"), "never the Debug identifier");

    let lines = screen_lines(&view, &NavState::new(Screen::OpenLots));
    assert!(
        lines
            .iter()
            .any(|l| l.text().starts_with("showing") && !l.text().contains("of book")),
        "no misleading n/a-of-book on a screen without a share column"
    );
    let lines = screen_lines(&view, &NavState::new(Screen::Positions));
    assert!(
        lines.iter().any(|l| l.text().contains("of book")),
        "Positions keeps the of-book segment"
    );
}

// ===========================================================================
// The owner-mandated data additions: basis + gain-% columns, the day-change
// column, the summary band's realized-YTD line, and the multi-row History chart.
// (TUI-VIEW-POS-004/005/006, TUI-VIEW-HIST-003)
// ===========================================================================

// @spec TUI-VIEW-POS-004
#[test]
fn positions_rows_carry_basis_and_gain_pct_of_basis() {
    let view = common::two_position_view();
    let nav = NavState::new(Screen::Positions);
    let (rows, _c, _h) = views::positions_rows(&view, &nav);
    let amzn = rows.iter().find(|r| r.label == "AMZN").unwrap();
    // 620 sh @ $50 basis = $31,000; unrealized $161,981.20 − $31,000 = $130,981.20
    // → 422.52% of basis → 4,225,200 ppm → "422.5%" (reports computes; views renders).
    assert_eq!(
        amzn.basis,
        Cents(31_000_00),
        "the total cost basis rides the row"
    );
    assert_eq!(
        amzn.gain_pct_ppm,
        Some(4_225_200),
        "the unr-% of basis from reports"
    );
    assert!(!amzn.gain_pct_na);

    let lines = screen_lines(&view, &nav);
    let line = lines
        .iter()
        .find(|l| l.text().contains("AMZN"))
        .unwrap()
        .text();
    assert!(line.contains("$31.0k"), "basis renders compact: {line}");
    assert!(line.contains("422.5%"), "gain % of basis renders: {line}");
}

// @spec TUI-VIEW-POS-004
#[test]
fn gain_pct_renders_na_on_zero_basis_and_dash_when_degraded() {
    // A priced zero-cost lot → gain-% `n/a` (basis ≤ 0), distinct from the
    // degraded `‡`; an unpriced row → the dash, while its basis still renders
    // (basis is reconstructable).
    let log = vec![
        buy(1, 18_000, "L-Z", "ZERO", 10, 0, "Robinhood"),
        buy(2, 18_100, "L-P", "PLTR", 10, 5_000, "Robinhood"),
    ];
    let snap = replay(&log, &[("ZERO", 6_000)]);
    let view = ViewBuilder::new(snap)
        .mark("ZERO", 6_000, Date(20_000))
        .estimate("ZERO", common::est("ZERO", 600_00, 120_00))
        .degraded("PLTR", sheets_view::DegradeReason::Permanent)
        .build();
    let nav = NavState::new(Screen::Positions);
    let (rows, _c, _h) = views::positions_rows(&view, &nav);
    let zero = rows.iter().find(|r| r.label == "ZERO").unwrap();
    assert!(!zero.degraded, "the zero-basis row is priced, not degraded");
    assert!(
        zero.gain_pct_na && zero.gain_pct_ppm.is_none(),
        "basis ≤ 0 → n/a"
    );
    let pltr = rows.iter().find(|r| r.label == "PLTR").unwrap();
    assert!(pltr.degraded && pltr.gain_pct_ppm.is_none() && !pltr.gain_pct_na);
    assert_eq!(pltr.basis, Cents(500_00), "basis survives degradation");

    let lines = screen_lines(&view, &nav);
    let zero_line = lines
        .iter()
        .find(|l| l.text().contains("ZERO"))
        .unwrap()
        .text();
    assert!(
        zero_line.contains("n/a"),
        "the zero-basis gain-% cell reads n/a: {zero_line}"
    );
    assert!(
        !zero_line.contains('\u{2021}'),
        "n/a is distinct from the degraded ‡: {zero_line}"
    );
    let pltr_line = lines
        .iter()
        .find(|l| l.text().contains("PLTR"))
        .unwrap()
        .text();
    assert!(
        pltr_line.contains("$500"),
        "basis renders even on a degraded row: {pltr_line}"
    );
}

// @spec TUI-VIEW-POS-005
#[test]
fn day_change_uses_the_latest_prior_capture_with_per_symbol_honesty() {
    // The real-shaped history: a key-0 garbage point (a "no priced day" capture)
    // plus an older complete point plus the latest prior capture, which is
    // INCOMPLETE (PENNY unpriced that day). The prior is the LATEST prior
    // capture — never the key-0 garbage, never the older complete point (that
    // would silently widen the "day") — and honesty is per symbol: AMZN (priced
    // at both endpoints) populates while PENNY (missing from the prior) dashes
    // its own row alone.
    let log = vec![
        buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood"),
        buy(2, 18_100, "L-F", "PENNY", 80_000, 3, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126), ("PENNY", 4)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 26_126, Date(20_000))
        .mark("PENNY", 4, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .estimate("PENNY", common::est("PENNY", 1_00, 20))
        // A key-0 garbage capture — never selected as the prior.
        .history_point(series_point(
            TradingDayKey(Date(0)),
            9_999_00,
            &[("AMZN", 9_999_00)],
            false,
        ))
        // An older COMPLETE capture — not the prior either.
        .history_point(series_point(
            TradingDayKey(Date(19_998)),
            6_200_00,
            &[("AMZN", 2_400_00), ("PENNY", 3_800_00)],
            false,
        ))
        // The LATEST prior capture: incomplete (no PENNY value) — still the prior.
        .history_point(series_point(
            TradingDayKey(Date(19_999)),
            2_500_00,
            &[("AMZN", 2_500_00)],
            true,
        ))
        .build();
    let nav = NavState::new(Screen::Positions);
    let (rows, _c, _h) = views::positions_rows(&view, &nav);
    let amzn = rows.iter().find(|r| r.label == "AMZN").unwrap();
    // Current 10 sh @ $261.26 = $2,612.60 vs the latest prior's $2,500.00.
    assert_eq!(
        amzn.day_delta,
        Some(Cents(112_60)),
        "vs the LATEST prior capture"
    );
    assert_eq!(
        amzn.day_delta_ppm,
        Some(45_040),
        "the % relative to the prior value"
    );
    let penny = rows.iter().find(|r| r.label == "PENNY").unwrap();
    assert_eq!(
        penny.day_delta, None,
        "a symbol the prior lacks dashes its own row only — never a delta vs an older day"
    );

    let lines = screen_lines(&view, &nav);
    let line = lines
        .iter()
        .find(|l| l.text().contains("AMZN"))
        .unwrap()
        .text();
    assert!(
        line.contains("+$113"),
        "the $ day change renders (compact): {line}"
    );
    assert!(
        line.contains("(+4.5%)"),
        "the % rides the same delta cell: {line}"
    );
    let fline = lines
        .iter()
        .find(|l| l.text().contains("PENNY"))
        .unwrap()
        .text();
    assert!(
        fline.contains('\u{2014}'),
        "the PENNY day-change cell holds the dash: {fline}"
    );
}

// @spec TUI-VIEW-POS-005
#[test]
fn day_change_is_a_dash_until_a_prior_capture_exists() {
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let nav = NavState::new(Screen::Positions);

    // No captures at all → the dash.
    let no_history = ViewBuilder::new(snap.clone())
        .mark("AMZN", 26_126, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .build();
    let (rows, _c, _h) = views::positions_rows(&no_history, &nav);
    assert_eq!(rows[0].day_delta, None);
    assert_eq!(rows[0].day_delta_ppm, None);

    // Today's own capture is not a prior: still the dash.
    let only_today = ViewBuilder::new(snap.clone())
        .mark("AMZN", 26_126, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .history_point(series_point(
            TradingDayKey(Date(20_000)),
            2_612_60,
            &[("AMZN", 2_612_60)],
            false,
        ))
        .build();
    let (rows, _c, _h) = views::positions_rows(&only_today, &nav);
    assert_eq!(
        rows[0].day_delta, None,
        "today's capture alone is not a prior"
    );
    let lines = screen_lines(&only_today, &nav);
    let line = lines
        .iter()
        .find(|l| l.text().contains("AMZN"))
        .unwrap()
        .text();
    assert!(
        line.contains('\u{2014}'),
        "the dash holds the day-change cell: {line}"
    );

    // A lone key-0 garbage capture is "no priced day", not a prior: the dash —
    // never a delta fabricated against nothing.
    let only_garbage = ViewBuilder::new(snap)
        .mark("AMZN", 26_126, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .history_point(series_point(
            TradingDayKey(Date(0)),
            9_999_00,
            &[("AMZN", 9_999_00)],
            false,
        ))
        .build();
    let (rows, _c, _h) = views::positions_rows(&only_garbage, &nav);
    assert_eq!(rows[0].day_delta, None, "a key-0 point is never the prior");
}

// @spec TUI-VIEW-POS-006
#[test]
fn summary_band_carries_the_realized_ytd_line_for_the_current_tax_year() {
    // A 2026 sale (gain $4,989) + a 2025 sale (excluded): the REALIZED 2026 YTD
    // line carries only the current tax year's realized P&L, gain-tinted with the
    // redundant ▲/+ treatment.
    let sell = |seq: u64, date: i32, sale: &str| ledger_core::LedgerEvent {
        id: format!("e{seq}"),
        seq: pt_core::Seq(seq),
        date: Date(date),
        kind: ledger_core::LedgerEventKind::Sell {
            sale_id: sale.to_string(),
            symbol: "AMZN".to_string(),
            qty: common::sh(100),
            unit_price_cents: Cents(9_989),
            fees_cents: Cents(0),
            lot_refs: vec![ledger_core::LotRef {
                lot_id: "L-A".to_string(),
                qty: common::sh(100),
            }],
            accrues_to_state: None,
            platform: "Robinhood".to_string(),
            tracking_code: None,
        },
    };
    let log = vec![
        buy(1, 18_000, "L-A", "AMZN", 300, 5_000, "Robinhood"), // 100 sh stay open
        sell(2, 20_100, "S-2025"),                              // calendar 2025 — excluded
        sell(3, 20_500, "S-2026"),                              // calendar 2026 — the YTD line
    ];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 26_126, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .build();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);
    let ytd = lines
        .iter()
        .find(|l| l.text().starts_with("REALIZED 2026 YTD"))
        .expect("the realized-YTD band line");
    assert!(
        ytd.text().contains("+$4,989"),
        "only the 2026 sale, whole dollars: {}",
        ytd.text()
    );
    assert!(
        !ytd.text().contains("9,978"),
        "the 2025 sale is excluded: {}",
        ytd.text()
    );
    assert!(
        ytd.text().contains('\u{25B2}'),
        "the gain carries ▲: {}",
        ytd.text()
    );
    assert_eq!(ytd.span_role("\u{25B2}"), Some(tui::theme::Role::Gain));
    assert_eq!(
        ytd.span_role("REALIZED"),
        Some(tui::theme::Role::FgFaint),
        "tracked-caps label"
    );
    // The band line sits above the double-entry rule with the other band lines.
    let ytd_idx = lines
        .iter()
        .position(|l| l.text().starts_with("REALIZED"))
        .unwrap();
    let rule_idx = lines
        .iter()
        .position(|l| l.text().starts_with('\u{2550}'))
        .unwrap();
    assert!(ytd_idx < rule_idx, "the YTD line rides the summary band");
}

// @spec TUI-VIEW-POS-006
#[test]
fn realized_ytd_renders_exact_zero_when_nothing_sold_and_survives_an_unpriced_book() {
    // No sales → the exact flat `· $0` (realized P&L is reconstructable — a
    // true zero, never the dash the mark-dependent figures use).
    let view = common::two_position_view();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);
    let ytd = lines
        .iter()
        .find(|l| l.text().starts_with("REALIZED 2026 YTD"))
        .expect("the realized-YTD line");
    assert!(
        ytd.text().contains("\u{00B7} $0"),
        "the exact flat zero: {}",
        ytd.text()
    );
    assert!(
        !ytd.text().contains('\u{2014}'),
        "never a dash: {}",
        ytd.text()
    );

    // An all-unpriced book still renders the line — it is mark-independent.
    let log = vec![buy(1, 18_000, "a", "AMZN", 10, 5_000, "Robinhood")];
    let snap = replay(&log, &[]);
    let unpriced = ViewBuilder::new(snap)
        .degraded("AMZN", sheets_view::DegradeReason::Permanent)
        .build();
    let lines = screen_lines(&unpriced, &nav);
    assert!(
        lines
            .iter()
            .any(|l| l.text().starts_with("REALIZED 2026 YTD")),
        "the realized line survives an unpriced book"
    );
}

// @spec TUI-VIEW-HIST-003, TUI-VIEW-HIST-004
#[test]
fn history_renders_a_multi_row_chart_with_axis_labels_and_date_span() {
    // Rising series $1,000 → $2,000 with an in-band gap and an incomplete day.
    let snap = ledger_core::Snapshot::default();
    let view = ViewBuilder::new(snap.clone())
        .history_point(series_point(TradingDayKey(Date(100)), 1_000_00, &[], false))
        .calendar_day(TradingDayKey(Date(101))) // a gap
        .history_point(series_point(TradingDayKey(Date(102)), 1_500_00, &[], true)) // incomplete
        .history_point(series_point(TradingDayKey(Date(103)), 2_000_00, &[], false))
        .build();
    let nav = NavState::new(Screen::History);
    let lines = screen_lines(&view, &nav);

    // Multi-row: several chart rows behind the ┤ axis gutter; the max labels the
    // top row and the min the bottom row.
    let chart: Vec<&tui::ScreenLine> = lines
        .iter()
        .filter(|l| l.text().contains('\u{2524}'))
        .collect();
    assert!(
        chart.len() >= 4,
        "a multi-row chart, got {} rows",
        chart.len()
    );
    // Axis labels are glance money — the compact form. (TUI-VIEW-HIST-004)
    assert!(
        chart.first().unwrap().text().contains("$2.0k"),
        "the max labels the top row: {}",
        chart.first().unwrap().text()
    );
    assert!(
        chart.last().unwrap().text().contains("$1.0k"),
        "the min labels the bottom row: {}",
        chart.last().unwrap().text()
    );

    // The date-span line beneath the chart names the span.
    assert!(
        lines
            .iter()
            .any(|l| l.text().contains("day 100") && l.text().contains("day 103")),
        "the date span renders"
    );

    // The bottom row: every captured column has a cell; the gap day is a blank
    // column; the today tick (the latest captured column) is bold in the gain
    // tint; the incomplete day's column carries the degraded role.
    let bottom = chart.last().unwrap();
    let data: String = bottom
        .text()
        .chars()
        .skip_while(|c| *c != '\u{2524}')
        .skip(1)
        .collect();
    let cells: Vec<char> = data.chars().collect();
    let ramps = cells
        .iter()
        .filter(|c| tui::theme::SPARK_RAMP.contains(c))
        .count();
    assert_eq!(
        ramps, 3,
        "three captured columns on the bottom row: {data:?}"
    );
    assert_eq!(
        cells[2], ' ',
        "the gap day renders a blank column: {data:?}"
    );
    let is_ramp = |s: &&tui::Seg| s.text.chars().any(|c| tui::theme::SPARK_RAMP.contains(&c));
    let tick = bottom
        .spans
        .iter()
        .rev()
        .find(is_ramp)
        .expect("a data cell");
    assert!(
        tick.bold,
        "the latest captured column is bold (the today tick)"
    );
    assert_eq!(tick.role, tui::theme::Role::Gain, "direction tint");
    assert!(
        chart.iter().any(
            |l| l.spans.iter().any(|s| s.role == tui::theme::Role::Degraded
                && s.text.chars().any(|c| tui::theme::SPARK_RAMP.contains(&c)))
        ),
        "the incomplete day's column is ‡-tinted (degraded role)"
    );

    // All-incomplete: the chart (the ┤ gutter) is suppressed per TUI-VIEW-HIST-002.
    let vi = ViewBuilder::new(snap)
        .history_point(series_point(TradingDayKey(Date(100)), 1_000_00, &[], true))
        .history_point(series_point(TradingDayKey(Date(101)), 1_100_00, &[], true))
        .build();
    let lines = screen_lines(&vi, &nav);
    assert!(
        !lines.iter().any(|l| l.text().contains('\u{2524}')),
        "the chart is suppressed when all-incomplete"
    );
}

// ===========================================================================
// The owner feedback pass: semantic colour on the data figures, the Realized
// ledger grid, and the company-name column. (TUI-VIEW-POS-008/009,
// TUI-VIEW-REAL-002/003, TUI-VIEW-LOT-003)
// ===========================================================================

// @spec TUI-VIEW-POS-008
#[test]
fn position_figures_carry_gain_loss_roles_and_net_carries_the_estimate_role() {
    use reports::TradingDayKey as TDK;
    use tui::testkit::{series_point as sp, ViewBuilder};
    // AMZN gains (bought $50, marked $261.26) with a prior capture above today's
    // value → a LOSS day-change beside a GAIN unrealized, so each figure's role
    // demonstrably follows ITS OWN sign, not the row's.
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 26_126, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .history_point(sp(
            TDK(Date(19_998)),
            3_000_00,
            &[("AMZN", 3_000_00)],
            false,
        ))
        .build();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);
    let amzn = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();

    // The unrealized figure rides the gain role (sign + ▲ stay the redundant cue).
    assert_eq!(amzn.span_role("\u{25B2}"), Some(tui::theme::Role::Gain));
    // The gain-% figure is SIGNED and carries the gain role on its own segment.
    let pct = amzn
        .spans
        .iter()
        .find(|s| s.text.contains('%') && s.text.contains('+') && !s.text.contains('$'))
        .expect("a signed gain-% cell");
    assert_eq!(
        pct.role,
        tui::theme::Role::Gain,
        "gain-% rides the gain role: {pct:?}"
    );
    // The day-change figure (value fell vs the prior capture) rides the LOSS role.
    assert_eq!(
        amzn.span_role("\u{25BC}"),
        Some(tui::theme::Role::Loss),
        "the day change carries its own sign's role: {}",
        amzn.text()
    );
    // The [est]-marked net figure rides the estimate role (not plain fg).
    let net = amzn
        .spans
        .iter()
        .zip(amzn.spans.iter().skip(1))
        .find(|(a, _)| a.text == "net ")
        .map(|(_, b)| b)
        .expect("the net cell follows its label");
    assert_eq!(
        net.role,
        tui::theme::Role::Estimate,
        "the net figure is estimate-tinted"
    );

    // The summary band's NET (POST-TAX) figure is estimate-tinted too.
    let band = lines
        .iter()
        .find(|l| l.text().starts_with("NET (POST-TAX)"))
        .unwrap();
    let figure = band.spans.iter().find(|s| s.text.contains('$')).unwrap();
    assert_eq!(figure.role, tui::theme::Role::Estimate);
    assert!(figure.bold, "the band figure keeps its tracked-caps bold");
}

// ===========================================================================
// The row Net is the post-tax VALUE — market value less the estimated
// unrealized tax — in exact whole dollars. (TUI-VIEW-POS-010/012)
// ===========================================================================

// @spec TUI-VIEW-POS-012, TUI-VIEW-POS-010
#[test]
fn net_equals_market_value_when_price_sits_below_the_fmv_basis() {
    // The FMV-basis RSU shape: 750 RSU shares whose basis is the FMV at vest
    // ($200.00/sh — already taxed as W-2 ordinary income at vest), marked
    // $190.27 → market value $142,702.50, unrealized −$7,297.50 (a LOSS), so
    // the estimated unrealized tax is zero and Net — the post-tax value —
    // EQUALS the market value: never value × (1 − rate), the legacy sheet's
    // $0-basis double-tax. Rendered as exact whole dollars ($142,702.50
    // rounds half-to-even to $142,702), never the compact $142.7k.
    let log = vec![
        buy(1, 20_042, "RSU-GRNT1-f", "AMZN", 500, 20_000, "Fidelity"),
        buy(2, 20_042, "RSU-GRNT2-b", "AMZN", 250, 20_000, "Fidelity"),
    ];
    let snap = replay(&log, &[("AMZN", 19_027)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 19_027, Date(20_615))
        .estimate("AMZN", common::est("AMZN", -729_750, 0))
        .build();
    let nav = NavState::new(Screen::Positions);

    let (rows, _c, _h) = views::positions_rows(&view, &nav);
    let amzn = rows.iter().find(|r| r.label == "AMZN").unwrap();
    assert_eq!(
        amzn.market_value,
        Some(Cents(14_270_250)),
        "750 sh @ $190.27"
    );
    assert_eq!(
        amzn.basis,
        Cents(15_000_000),
        "basis = FMV at vest: 750 × $200.00"
    );
    assert_eq!(
        amzn.unrealized_pretax,
        Some(Cents(-729_750)),
        "a $7,297.50 loss"
    );
    assert_eq!(
        amzn.net_post_tax, amzn.market_value,
        "zero estimated tax → Net (post-tax value) == market value"
    );

    let lines = screen_lines(&view, &nav);
    let line = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();
    // The NET cell renders exact whole dollars in the estimate role.
    let net = line
        .spans
        .iter()
        .zip(line.spans.iter().skip(1))
        .find(|(a, _)| a.text == "net ")
        .map(|(_, b)| b)
        .expect("the net cell follows its label");
    assert_eq!(
        net.text.trim(),
        "$142,702",
        "Net is the market value in exact whole dollars — not −$7.3k, not a rate-discounted value: {}",
        line.text()
    );
    assert_eq!(net.role, tui::theme::Role::Estimate);
    // The VALUE cell still compacts (glance money) — only NET joined the
    // exact-whole-dollars family.
    assert!(
        line.text().contains("$142.7k"),
        "VALUE stays compact: {}",
        line.text()
    );
}

// @spec TUI-VIEW-POS-008
#[test]
fn a_degraded_dash_keeps_the_dim_role_never_a_gain_loss_tint() {
    use tui::testkit::ViewBuilder;
    let log = vec![buy(1, 18_000, "L-P", "PLTR", 10, 5_000, "Robinhood")];
    let snap = replay(&log, &[]);
    let view = ViewBuilder::new(snap)
        .degraded("PLTR", sheets_view::DegradeReason::Permanent)
        .build();
    let lines = screen_lines(&view, &NavState::new(Screen::Positions));
    let pltr = lines.iter().find(|l| l.text().contains("PLTR")).unwrap();
    for seg in pltr.spans.iter().filter(|s| s.text.contains('\u{2014}')) {
        assert_eq!(
            seg.role,
            tui::theme::Role::Degraded,
            "a degraded dash stays dim: {seg:?}"
        );
    }
}

// @spec TUI-VIEW-REAL-002, TUI-VIEW-REAL-003, TUI-VIEW-REAL-004
#[test]
fn realized_renders_ledger_columns_under_a_title_row_with_gain_loss_roles() {
    let sell = |seq: u64, date: i32, sale: &str, price: i64| ledger_core::LedgerEvent {
        id: format!("e{seq}"),
        seq: pt_core::Seq(seq),
        date: Date(date),
        kind: ledger_core::LedgerEventKind::Sell {
            sale_id: sale.to_string(),
            symbol: "AMZN".to_string(),
            qty: common::sh(10),
            unit_price_cents: Cents(price),
            fees_cents: Cents(0),
            lot_refs: vec![ledger_core::LotRef {
                lot_id: "L-A".to_string(),
                qty: common::sh(10),
            }],
            accrues_to_state: None,
            platform: "Robinhood".to_string(),
            tracking_code: None,
        },
    };
    // A 2025 LOSS year and a 2026 GAIN year: each gain figure carries its own role.
    let log = vec![
        buy(1, 18_000, "L-A", "AMZN", 20, 5_000, "Robinhood"),
        sell(2, 20_100, "S-2025", 4_000), // loss −$100/sh × 10
        sell(3, 20_500, "S-2026", 9_989), // gain
    ];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = tui::testkit::ViewBuilder::new(snap).build();
    let nav = NavState::new(Screen::Realized);
    let lines = screen_lines(&view, &nav);

    // The title row rides the same grid as the data rows.
    let title = lines
        .iter()
        .find(|l| l.text().contains("PERIOD"))
        .expect("the title row");
    for t in ["PERIOD", "PROCEEDS", "BASIS", "GAIN"] {
        assert!(
            title.text().contains(t),
            "the {t} title renders: {}",
            title.text()
        );
    }
    assert!(
        title
            .spans
            .iter()
            .all(|s| s.role == tui::theme::Role::FgFaint || s.text.trim().is_empty()),
        "the title row is tracked-caps faint: {title:?}"
    );
    let rule_pos = |l: &tui::ScreenLine| -> Vec<usize> {
        l.text()
            .chars()
            .enumerate()
            .filter(|(_, c)| *c == '│')
            .map(|(i, _)| i)
            .collect()
    };
    let y2025 = lines
        .iter()
        .find(|l| l.text().contains("2025"))
        .expect("the 2025 row");
    let y2026 = lines
        .iter()
        .find(|l| l.text().contains("2026"))
        .expect("the 2026 row");
    assert!(
        rule_pos(title).len() >= 3,
        "│-ruled columns: {}",
        title.text()
    );
    assert_eq!(
        rule_pos(title),
        rule_pos(y2025),
        "the title aligns:\n{}\n{}",
        title.text(),
        y2025.text()
    );
    assert_eq!(rule_pos(y2025), rule_pos(y2026), "the rows share one grid");
    // The title precedes the data rows.
    let ti = lines
        .iter()
        .position(|l| l.text().contains("PERIOD"))
        .unwrap();
    let ri = lines
        .iter()
        .position(|l| l.text().contains("2025"))
        .unwrap();
    assert!(ti < ri, "the title row sits above the data");

    // Each year's gain figure carries its own role with the redundant glyph.
    assert_eq!(
        y2025.span_role("\u{25BC}"),
        Some(tui::theme::Role::Loss),
        "{}",
        y2025.text()
    );
    assert_eq!(
        y2026.span_role("\u{25B2}"),
        Some(tui::theme::Role::Gain),
        "{}",
        y2026.text()
    );

    // Realized figures are glance money: the compact whole-dollar form.
    // (TUI-VIEW-REAL-004)
    assert!(
        y2026.text().contains("$999"),
        "proceeds compact ($998.90 → $999): {}",
        y2026.text()
    );
    assert!(
        y2026.text().contains("+$499"),
        "gain compact: {}",
        y2026.text()
    );
    assert!(
        !y2026.text().contains(".90"),
        "no cents on a glance figure: {}",
        y2026.text()
    );
}

// @spec TUI-VIEW-TAX-004
#[test]
fn tax_amounts_and_reserves_render_exact_whole_dollars_never_abbreviated() {
    use config::{Jurisdiction, TaxYear};
    // Tax money is reconciliation money: a $2,400 accrual and a five-figure
    // reserve render as exact whole dollars — never `$2.4k` / `$25.0k`, never
    // trailing cents.
    let view = tui::testkit::ViewBuilder::new(ledger_core::Snapshot::default())
        .accruals(vec![tui::testkit::accrual_moved("S1", "L1")])
        .annual_rows(vec![tax::AnnualRow {
            jurisdiction: Jurisdiction::Federal,
            tax_year: TaxYear(2026),
            accrued_cents: Cents(24_968_00),
            moved_cents: Cents(19_950_00),
            paid_cents: Cents(0),
            outstanding_cents: Cents(24_968_00),
            shortfall_cents: Cents(5_018_00),
            gain_cents: Cents(100_000_00),
            effective_rate_ppm: None,
            bracket_state: config::BracketState::Verified,
        }])
        .build();
    let nav = NavState::new(Screen::TaxReserves);
    let lines = screen_lines(&view, &nav);

    let row = lines
        .iter()
        .find(|l| l.text().contains("S1/L1"))
        .expect("the accrual row")
        .text();
    assert!(
        row.contains("$2,400"),
        "the accrual amount in exact whole dollars: {row}"
    );
    assert!(
        !row.contains("$2,400.00") && !row.contains("$2.4k"),
        "never cents, never compact: {row}"
    );

    let res = lines
        .iter()
        .find(|l| l.text().starts_with("RESERVE"))
        .expect("the reserve line")
        .text();
    assert!(
        res.contains("accrued $24,968"),
        "exact whole-dollar reserve: {res}"
    );
    assert!(
        res.contains("moved $19,950"),
        "exact whole-dollar moved: {res}"
    );
    assert!(
        res.contains("shortfall $5,018"),
        "exact whole-dollar shortfall: {res}"
    );
    assert!(
        !res.contains("$25.0k") && !res.contains(".00"),
        "never compact, never cents: {res}"
    );
}

// @spec TUI-VIEW-TAX-003
#[test]
fn tax_accrual_rows_render_under_an_aligned_title_row() {
    let view = tui::testkit::ViewBuilder::new(ledger_core::Snapshot::default())
        .accruals(vec![tui::testkit::accrual_moved("S1", "L1")])
        .build();
    let nav = NavState::new(Screen::TaxReserves);
    let lines = screen_lines(&view, &nav);
    let title = lines
        .iter()
        .find(|l| l.text().contains("SALE/LOT"))
        .expect("the title row");
    for t in ["SALE/LOT", "AMOUNT", "STATUS"] {
        assert!(
            title.text().contains(t),
            "the {t} title renders: {}",
            title.text()
        );
    }
    assert_eq!(title.span_role("SALE/LOT"), Some(tui::theme::Role::FgFaint));
    // The title's amount cell right-aligns over the row's amount cell: the `│`
    // rule sits at the same position.
    let row = lines
        .iter()
        .find(|l| l.text().contains("S1/L1"))
        .expect("the accrual row");
    let rule = |l: &tui::ScreenLine| l.text().chars().position(|c| c == '│');
    assert_eq!(
        rule(title),
        rule(row),
        "the title aligns:\n{}\n{}",
        title.text(),
        row.text()
    );
    // The title precedes the group header + rows.
    let ti = lines
        .iter()
        .position(|l| l.text().contains("SALE/LOT"))
        .unwrap();
    let ri = lines
        .iter()
        .position(|l| l.text().contains("S1/L1"))
        .unwrap();
    assert!(ti < ri);
}
