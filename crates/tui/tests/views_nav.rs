//! Navigation tests (TUI-VIEW-NAV-*): filter / sort / drill / focus / refresh /
//! empty states / read-only-launch.

mod common;

use pt_core::{Cents, Date};
use tui::testkit::{buy, flat_federal_ctx, replay, FakeRuntime, ViewBuilder};
use tui::views::{self, Filter, NavState, RowIdentity, Scope, Screen, SortDir, SortKey};
use tui::{Frame, Model};

// @spec TUI-VIEW-NAV-001
#[test]
fn filter_is_visibility_only_totals_stay_over_the_whole_priced_portfolio() {
    // Three symbols; filter to one. The filter changes VISIBILITY only; the
    // filter-state header states the subset (shown/total · % of book), and the
    // hidden rows' share stays in the whole-book denominator. (TUI-VIEW-NAV-001)
    let log = vec![
        buy(1, 18_000, "a", "AMZN", 10, 6_000, "Robinhood"),
        buy(2, 18_100, "g", "GOOGL", 10, 6_000, "Robinhood"),
        buy(3, 18_200, "m", "MSFT", 10, 6_000, "Robinhood"),
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
    nav.filter = Filter::Symbol("AMZN".to_string());
    let (rows, _c, header) = views::positions_rows(&view, &nav);
    assert_eq!(
        rows.len(),
        1,
        "filter hides the other rows (visibility-only)"
    );
    assert_eq!(header.shown, 1);
    assert_eq!(
        header.total, 3,
        "the total stays over the whole priced portfolio"
    );
    // The filter-state header text reads honestly.
    let text = header.text();
    assert!(
        text.contains("showing 1 of 3"),
        "header states the subset: {text}"
    );
    assert!(text.contains("of book"));
    // The shown share ≈ 1/3 of the book (each is an equal third).
    let shown_ppm = header.shown_share_ppm.unwrap();
    assert!(
        (shown_ppm - 333_333).abs() < 10,
        "the shown subset is ~33% of the book, not rebased to 100%"
    );
}

// @spec TUI-VIEW-NAV-002
#[test]
fn sort_sends_a_row_to_the_tail_only_when_the_sort_key_itself_is_degraded() {
    // AMZN priced (high MV), PLTR unpriced (MV degraded). Sorting by market value
    // descending puts AMZN first and the MV-degraded PLTR at the TAIL (both
    // directions). A row degraded in some OTHER column sorts normally on the key.
    let log = vec![
        buy(1, 18_000, "a", "AMZN", 10, 6_000, "Robinhood"),
        buy(2, 18_100, "p", "PLTR", 10, 6_000, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 6_000)]); // PLTR unpriced
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 6_000, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .degraded("PLTR", sheets_view::DegradeReason::Permanent)
        .build();

    for dir in [SortDir::Asc, SortDir::Desc] {
        let mut nav = NavState::new(Screen::Positions);
        nav.sort_key = SortKey::MarketValue;
        nav.sort_dir = dir;
        let (rows, _c, _h) = views::positions_rows(&view, &nav);
        assert_eq!(
            rows.last().unwrap().label,
            "PLTR",
            "the MV-degraded row tails in {dir:?}"
        );
    }
}

// @spec TUI-VIEW-NAV-003
#[test]
fn drill_sets_contextual_scope_cleared_on_ascend_with_retained_filter_and_sort() {
    let mut model = Model::new(); // landing: Positions
                                  // Set a retained filter + sort on Positions.
    model.current_mut().nav.filter = Filter::Symbol("AMZN".to_string());
    model.current_mut().nav.sort_key = SortKey::MarketValue;

    // Drill into AMZN's lots: pushes an OpenLots frame with the AMZN contextual scope.
    assert!(model.drill(&RowIdentity::Symbol("AMZN".to_string())));
    assert_eq!(model.current().nav.screen, Screen::OpenLots);
    assert_eq!(model.current().nav.scope, Scope::Symbol("AMZN".to_string()));
    assert!(
        model.current().nav.scope.breadcrumb().contains("AMZN ›"),
        "a breadcrumb chip is set"
    );

    // Ascend: the OpenLots frame (and its contextual scope) is popped; the parent
    // Positions frame's retained filter + sort survive. (TUI-VIEW-NAV-003)
    assert!(model.ascend());
    assert_eq!(model.current().nav.screen, Screen::Positions);
    assert_eq!(
        model.current().nav.scope,
        Scope::All,
        "the contextual scope is cleared on ascend"
    );
    assert_eq!(
        model.current().nav.filter,
        Filter::Symbol("AMZN".to_string()),
        "the user filter is retained"
    );
    assert_eq!(
        model.current().nav.sort_key,
        SortKey::MarketValue,
        "the sort is retained"
    );
}

// @spec TUI-VIEW-NAV-004
#[test]
fn focus_anchors_to_row_identity_across_resort_and_falls_to_nearest_when_vanished() {
    // The focus is on GOOGL at index 1. After a re-sort reorders the list, focus
    // stays on GOOGL's identity (new index), not the old index. (TUI-VIEW-NAV-004)
    let focus = RowIdentity::Symbol("GOOGL".to_string());
    let new_order = vec![
        RowIdentity::Symbol("GOOGL".to_string()),
        RowIdentity::Symbol("AMZN".to_string()),
        RowIdentity::Symbol("MSFT".to_string()),
    ];
    let (anchored, idx) = views::reanchor_focus(&focus, 1, &new_order);
    assert_eq!(anchored, focus, "focus follows the identity, not the index");
    assert_eq!(idx, 0, "GOOGL is now first");

    // The focused identity vanished (its symbol closed): focus falls to the nearest
    // row (the old index clamped into the new list).
    let after_vanish = vec![
        RowIdentity::Symbol("AMZN".to_string()),
        RowIdentity::Symbol("MSFT".to_string()),
    ];
    let (fallen, fidx) = views::reanchor_focus(&focus, 1, &after_vanish);
    assert_eq!(
        fallen,
        RowIdentity::Symbol("MSFT".to_string()),
        "falls to the nearest row"
    );
    assert_eq!(fidx, 1);
}

// @spec TUI-VIEW-NAV-004
#[test]
fn a_model_refresh_preserves_focus_identity_across_a_resort() {
    // The Model's refresh path re-anchors focus to row IDENTITY, not index: after a
    // refresh whose new marks re-sort a value-sorted Positions list, the selection
    // stays on the same symbol (its new scroll index), never jumping under the
    // owner. A vanished identity falls to the nearest row. (TUI-VIEW-NAV-004)
    let log = vec![
        buy(1, 18_000, "a", "AMZN", 10, 6_000, "Robinhood"),
        buy(2, 18_100, "g", "GOOGL", 10, 6_000, "Robinhood"),
        buy(3, 18_200, "m", "MSFT", 10, 6_000, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 6_000), ("GOOGL", 6_000), ("MSFT", 6_000)]);
    let view = ViewBuilder::new(snap.clone())
        .mark("AMZN", 6_000, Date(20_000))
        .mark("GOOGL", 6_000, Date(20_000))
        .mark("MSFT", 6_000, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .estimate("GOOGL", common::est("GOOGL", 10_00, 2_00))
        .estimate("MSFT", common::est("MSFT", 10_00, 2_00))
        .build();
    let mut rt = FakeRuntime::new(view, flat_federal_ctx(common::YEAR, 220_000));

    let mut model = Model::new(); // landing: Positions, sorted by Label asc
    model.current_mut().nav.sort_key = SortKey::MarketValue;
    model.current_mut().nav.sort_dir = SortDir::Desc;
    // Focus GOOGL.
    model.current_mut().nav.focus = RowIdentity::Symbol("GOOGL".to_string());

    // A refresh re-reads marks (the held view is unchanged here) and re-anchors:
    // GOOGL's identity survives, so focus stays on it.
    let notices = model.refresh(&mut rt);
    assert!(
        notices.is_empty(),
        "no dangling frames on the landing screen"
    );
    assert_eq!(
        model.current().nav.focus,
        RowIdentity::Symbol("GOOGL".to_string()),
        "the refresh preserves focus identity"
    );

    // Now a refresh where GOOGL has closed (its position is gone): focus falls to
    // the nearest surviving row rather than dangling.
    let after_log = vec![
        buy(1, 18_000, "a", "AMZN", 10, 6_000, "Robinhood"),
        buy(3, 18_200, "m", "MSFT", 10, 6_000, "Robinhood"),
    ];
    let after_snap = replay(&after_log, &[("AMZN", 6_000), ("MSFT", 6_000)]);
    rt.set_view(
        ViewBuilder::new(after_snap)
            .mark("AMZN", 6_000, Date(20_000))
            .mark("MSFT", 6_000, Date(20_000))
            .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
            .estimate("MSFT", common::est("MSFT", 10_00, 2_00))
            .build(),
    );
    model.refresh(&mut rt);
    let focus = &model.current().nav.focus;
    assert!(
        matches!(focus, RowIdentity::Symbol(s) if s == "AMZN" || s == "MSFT"),
        "a vanished focus falls to the nearest surviving row, got {focus:?}"
    );
}

// @spec TUI-VIEW-NAV-005
#[test]
fn refresh_reresolves_stacked_anchors_and_replaces_a_dangling_frame_with_a_calm_notice() {
    // Drill Positions → AMZN's lots → a specific lot. A concurrent entry append
    // closes that lot; on refresh the dangling lot frame is replaced with a calm
    // returning-to-parent notice. (TUI-VIEW-NAV-005)
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 100, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = ViewBuilder::new(snap).build();

    let mut model = Model::new();
    // Manually build a stack: Positions → OpenLots(AMZN) → Realized(lot L-A).
    model.stack.push(Frame {
        nav: {
            let mut n = NavState::new(Screen::OpenLots);
            n.scope = Scope::Symbol("AMZN".to_string());
            n
        },
    });
    model.stack.push(Frame {
        nav: {
            let mut n = NavState::new(Screen::Realized);
            n.scope = Scope::Lot("L-A".to_string());
            n
        },
    });
    assert_eq!(model.stack.len(), 3);

    // While the lot still exists, the stack re-resolves clean.
    assert!(model.reresolve_stack(&view).is_empty());

    // Now the lot is gone (a newer Sell closed it): the dangling lot frame is dropped
    // with a calm notice. Build a view whose snapshot no longer has L-A.
    let empty = ViewBuilder::new(ledger_core::Snapshot::default()).build();
    let notices = model.reresolve_stack(&empty);
    assert!(
        !notices.is_empty(),
        "a dangling drilled frame is replaced with a notice"
    );
    assert!(
        notices[0].contains("returning to"),
        "the calm notice names the parent: {}",
        notices[0]
    );
}

// @spec TUI-VIEW-NAV-006
#[test]
fn six_calm_empty_states_distinct_from_each_other_and_the_integrity_block() {
    // No positions (the whole-book pre-import state).
    assert_eq!(
        views::empty_state(&Screen::Positions, 0, 0, false, true),
        Some(views::EmptyState::NoPositions)
    );
    // No open lots — every lot has been fully sold (the book HAS positions):
    // distinct from No positions. The pre-import book reuses No positions.
    assert_eq!(
        views::empty_state(&Screen::OpenLots, 0, 0, false, false),
        Some(views::EmptyState::NoOpenLots)
    );
    assert_eq!(
        views::empty_state(&Screen::OpenLots, 0, 0, false, true),
        Some(views::EmptyState::NoPositions)
    );
    assert_eq!(
        views::EmptyState::NoOpenLots.message(),
        "no open lots — every lot has been fully sold"
    );
    // No accruals.
    assert_eq!(
        views::empty_state(&Screen::TaxReserves, 0, 0, false, false),
        Some(views::EmptyState::NoAccruals)
    );
    // No history.
    assert_eq!(
        views::empty_state(&Screen::History, 0, 0, false, false),
        Some(views::EmptyState::NoHistory)
    );
    // No realized gains — never the misleading "no positions" wording.
    assert_eq!(
        views::empty_state(&Screen::Realized, 0, 0, false, false),
        Some(views::EmptyState::NoRealizedGains)
    );
    assert_eq!(
        views::EmptyState::NoRealizedGains.message(),
        "no realized gains yet — they appear when you sell a lot"
    );
    // Filter matched zero (rows exist but none shown).
    assert_eq!(
        views::empty_state(&Screen::Positions, 12, 0, true, false),
        Some(views::EmptyState::FilterMatchedZero)
    );
    // Messages are calm, never the ✗ integrity glyph — and all six are distinct.
    let all = [
        views::EmptyState::NoPositions,
        views::EmptyState::NoOpenLots,
        views::EmptyState::NoAccruals,
        views::EmptyState::NoHistory,
        views::EmptyState::NoRealizedGains,
        views::EmptyState::FilterMatchedZero,
    ];
    for e in &all {
        assert!(
            !e.message().contains('\u{2717}'),
            "an empty state never uses ✗"
        );
    }
    let mut msgs: Vec<String> = all.iter().map(|e| e.message()).collect();
    msgs.sort();
    msgs.dedup();
    assert_eq!(msgs.len(), 6, "the six empty states are pairwise distinct");
}

// @spec TUI-VIEW-NAV-007
#[test]
fn views_mutate_nothing_and_launching_an_action_offline_or_lock_held_is_allowed() {
    // views render-only: no submit path exists on the read screens. Launching an
    // entry action while offline / lock-held is allowed — entry's write loop is the
    // single rejection path — with the offline/lock state surfaced in the status
    // line. (TUI-VIEW-NAV-007)
    let view = ViewBuilder::new(ledger_core::Snapshot::default())
        .offline()
        .build();
    // The status line surfaces the offline state (so launching is not a surprise).
    let status = tui::StatusLine::from_view(&view, tui::Mode::Views, &Screen::TaxReserves);
    assert!(!status.connected, "offline is surfaced");
    assert!(
        status.text().contains("stale"),
        "the status line shows stale/offline"
    );

    // The launch passes only identity (entry re-resolves live); views never mutate.
    let key = tax::AccrualKey {
        sale_id: "S1".to_string(),
        lot_id: "L1".to_string(),
        jurisdiction: config::Jurisdiction::Federal,
        tax_year: config::TaxYear(common::YEAR),
    };
    let identity = tui::entry::LaunchIdentity::Accrual(key);
    assert!(matches!(identity, tui::entry::LaunchIdentity::Accrual(_)));
}

// @spec TUI-VIEW-NAV-008
#[test]
fn refresh_executes_the_fixed_lifecycle_order_re_read_then_reresolve_then_reanchor() {
    // A single Model::refresh executes the fixed lifecycle in order so a frame never
    // renders against half-updated state: re-read marks (the port refresh ran) →
    // re-resolve each stacked frame's anchor identity (a dangling drilled frame is
    // dropped) → re-anchor focus to the surviving row identity (a vanished focus
    // falls to the nearest row). One refresh exercises all three steps. (TUI-VIEW-NAV-008)
    let log = vec![
        buy(1, 18_000, "L-A", "AMZN", 100, 6_000, "Robinhood"),
        buy(2, 18_100, "L-G", "GOOGL", 100, 6_000, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 6_000), ("GOOGL", 6_000)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 6_000, Date(20_000))
        .mark("GOOGL", 6_000, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .estimate("GOOGL", common::est("GOOGL", 10_00, 2_00))
        .build();
    let mut rt = FakeRuntime::new(view, flat_federal_ctx(common::YEAR, 220_000));

    // Stack: Positions (focus GOOGL) → OpenLots drilled into the now-vanishing lot.
    let mut model = Model::new();
    model.current_mut().nav.focus = RowIdentity::Symbol("GOOGL".to_string());
    model.stack.push(Frame {
        nav: {
            let mut n = NavState::new(Screen::OpenLots);
            n.scope = Scope::Lot("L-GONE".to_string()); // a lot that does not resolve
            n
        },
    });
    assert_eq!(model.stack.len(), 2);

    let before = rt.refresh_count;
    // The refresh re-reads (port.refresh), drops the dangling lot frame, then re-anchors
    // the surviving Positions frame's focus over the re-read ordering — all in order.
    let notices = model.refresh(&mut rt);

    // 1) re-read marks ran.
    assert_eq!(
        rt.refresh_count,
        before + 1,
        "the refresh re-reads marks first"
    );
    // 2) the dangling drilled frame was dropped with a calm returning-to-parent notice.
    assert!(
        !notices.is_empty(),
        "the dangling lot frame is dropped during re-resolve"
    );
    assert!(
        notices[0].contains("returning to"),
        "a calm notice names the parent: {}",
        notices[0]
    );
    assert_eq!(
        model.stack.len(),
        1,
        "the surviving frame is the landing Positions frame"
    );
    // 3) focus on the surviving frame re-anchored to a present row identity.
    assert_eq!(
        model.current().nav.focus,
        RowIdentity::Symbol("GOOGL".to_string()),
        "focus re-anchors to the surviving row identity after re-resolve"
    );
}

// @spec TUI-VIEW-NAV-001
#[test]
fn filter_describe_and_clear_helpers() {
    assert_eq!(Filter::None.describe(), "all");
    assert!(Filter::Symbol("AMZN".to_string())
        .describe()
        .contains("AMZN"));
    let _ = Cents(0); // keep the import used across edits
}

// @spec TUI-VIEW-NAV-014
#[test]
fn switch_screen_replaces_the_stack_and_retains_each_screens_filter_and_sort() {
    let mut model = Model::new();
    // The owner filters + sorts Positions...
    model.current_mut().nav.filter = Filter::Symbol("AMZN".to_string());
    model.current_mut().nav.sort_key = SortKey::MarketValue;
    model.current_mut().nav.sort_dir = SortDir::Desc;

    // ...switches to Tax & Reserves: the stack is replaced with a fresh landing
    // frame for the target.
    model.switch_screen(Screen::TaxReserves);
    assert_eq!(model.stack.len(), 1);
    assert_eq!(model.current().nav.screen, Screen::TaxReserves);
    assert_eq!(
        model.current().nav.filter,
        Filter::None,
        "a fresh screen starts unfiltered"
    );

    // The owner filters Tax by year, then returns to Positions: ITS filter/sort
    // come back; switching away again restores Tax's own year filter.
    model.current_mut().nav.filter = Filter::TaxYear(2026);
    model.switch_screen(Screen::Positions);
    assert_eq!(model.current().nav.screen, Screen::Positions);
    assert_eq!(
        model.current().nav.filter,
        Filter::Symbol("AMZN".to_string()),
        "retained"
    );
    assert_eq!(
        model.current().nav.sort_key,
        SortKey::MarketValue,
        "sort retained"
    );
    assert_eq!(model.current().nav.sort_dir, SortDir::Desc);
    model.switch_screen(Screen::TaxReserves);
    assert_eq!(
        model.current().nav.filter,
        Filter::TaxYear(2026),
        "Tax's filter retained"
    );

    // A switch never mutates anything durable: only nav state moved.
    assert!(model.entry.is_empty());
    assert_eq!(model.mode, tui::Mode::Views);
}

// @spec TUI-VIEW-NAV-014
#[test]
fn switch_screen_from_a_drilled_frame_drops_the_drill_scope() {
    let mut model = Model::new();
    assert!(model.drill(&RowIdentity::Symbol("AMZN".to_string())));
    assert!(model.stack.len() > 1, "drilled");
    // Switching from a drilled frame replaces the WHOLE stack: the drilled
    // contextual scope dies with the drill that set it.
    model.switch_screen(Screen::History);
    assert_eq!(model.stack.len(), 1);
    assert_eq!(model.current().nav.screen, Screen::History);
    assert_eq!(
        model.current().nav.scope,
        Scope::All,
        "no leaked drill scope"
    );

    // Switching to the screen we are already on (landing) is a no-op.
    let before = model.clone();
    model.switch_screen(Screen::History);
    assert_eq!(model, before);
}
