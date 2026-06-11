//! Data-driven column sizing on the spans seam (views-design.md → "Line
//! Rendering"): widths computed from the rendered data under clamps, decimal
//! alignment, ellipsize-only-when-clamped, and the degraded dash sitting inside
//! its own column — pinned with the private-holding fixture (a 7-char unpriced
//! symbol, 24,150 shares) beside a six-figure value and a sub-dollar
//! 80,000 @ $0.04 position.

mod common;

use pt_core::Date;
use tui::testkit::{buy, replay, ViewBuilder};
use tui::views::{NavState, Screen};
use tui::{screen_lines, ScreenLine};

/// The char positions of every `│` column rule in a line's flat text.
fn rule_positions(line: &ScreenLine) -> Vec<usize> {
    line.text()
        .chars()
        .enumerate()
        .filter(|(_, c)| *c == '│')
        .map(|(i, _)| i)
        .collect()
}

/// The end (exclusive) char position of `needle` in the line's flat text.
fn end_of(line: &ScreenLine, needle: &str) -> usize {
    let text = line.text();
    let byte = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in {text}"));
    text[..byte].chars().count() + needle.chars().count()
}

/// The private-holding book: a 7-char unpriced symbol with 24,150 shares, a
/// six-figure priced value, and a sub-dollar 80,000-share position.
fn private_holding_view() -> tui::port::ViewState {
    let log = vec![
        buy(1, 18_000, "L-P", "PRIVHLD", 24_150, 1_000, "Robinhood"),
        buy(2, 18_100, "L-A", "AMZN", 620, 5_000, "Robinhood"),
        buy(3, 18_200, "L-F", "PENNY", 80_000, 3, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126), ("PENNY", 4)]);
    ViewBuilder::new(snap)
        .mark("AMZN", 26_126, Date(20_000))
        .mark("PENNY", 4, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 5_000_00, 1_000_00))
        .estimate("PENNY", common::est("PENNY", 100_00, 20_00))
        .degraded("PRIVHLD", sheets_view::DegradeReason::Permanent)
        .build()
}

// @spec TUI-VIEW-POS-001, TUI-VIEW-POS-010, TUI-VIEW-NAV-016
#[test]
fn position_columns_size_from_the_data_and_stay_aligned_across_extreme_rows() {
    let view = private_holding_view();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);

    let row = |sym: &str| {
        lines
            .iter()
            .find(|l| l.text().contains(sym))
            .unwrap_or_else(|| panic!("a row for {sym}"))
    };
    let amzn = row("AMZN");
    let penny = row("PENNY");
    let priv_row = row("PRIVHLD");

    // Every `│` column rule sits at the same char position on every row — the
    // 24,150-share, six-figure, and sub-dollar rows share one true ledger grid.
    let amzn_rules = rule_positions(amzn);
    assert!(
        amzn_rules.len() >= 5,
        "the │-ruled columns render: {}",
        amzn.text()
    );
    assert_eq!(
        rule_positions(penny),
        amzn_rules,
        "PENNY aligns:\n{}\n{}",
        amzn.text(),
        penny.text()
    );
    assert_eq!(
        rule_positions(priv_row),
        amzn_rules,
        "PRIVHLD aligns:\n{}\n{}",
        amzn.text(),
        priv_row.text()
    );

    // Share counts are thousands-grouped and decimal/right-aligned: 24,150 and
    // 80,000 end on the same column.
    assert_eq!(
        end_of(priv_row, "24,150"),
        end_of(penny, "80,000"),
        "share counts right-align in one column:\n{}\n{}",
        priv_row.text(),
        penny.text()
    );

    // The six-figure value compacts (glance money) while the four-cent price
    // keeps its per-share cents. (TUI-VIEW-POS-010, TUI-VIEW-NAV-016)
    assert!(
        amzn.text().contains("$162.0k"),
        "the six-figure value compacts: {}",
        amzn.text()
    );
    assert!(
        penny.text().contains("$0.04"),
        "the PENNY price keeps cents: {}",
        penny.text()
    );
}

// @spec TUI-VIEW-POS-001, TUI-VIEW-POS-003
#[test]
fn the_unpriced_dash_sits_in_its_column_and_the_word_marks_the_row() {
    let view = private_holding_view();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);
    let priv_row = lines.iter().find(|l| l.text().contains("PRIVHLD")).unwrap();
    let amzn = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();

    // The unpriced symbol is NOT ellipsized (7 chars fits the clamp), the row
    // keeps its real share count, every figure column holds the dash inside the
    // shared grid (rule positions pinned above), and the distinct unpriced word
    // trails the row — never a fabricated 0.
    let text = priv_row.text();
    assert!(
        text.contains("PRIVHLD"),
        "the 7-char symbol renders whole: {text}"
    );
    assert!(!text.contains('…'), "no ellipsis below the clamp: {text}");
    assert!(
        text.contains("24,150"),
        "the real share count renders: {text}"
    );
    assert!(
        text.contains('\u{2014}'),
        "the — dash holds the unpriced figures: {text}"
    );
    assert!(
        text.contains("‡ unpriced"),
        "the unpriced word marks the row: {text}"
    );
    assert!(!text.contains("$0"), "never a fabricated zero: {text}");

    // The dash occupies the same market-value column the priced row's figure
    // does: between the same pair of column rules.
    let rules = rule_positions(amzn);
    let mv_cell_start = rules[1];
    let mv_cell_end = rules[2];
    let dash_pos = priv_row
        .text()
        .chars()
        .enumerate()
        .filter(|(_, c)| *c == '\u{2014}')
        .map(|(i, _)| i)
        .find(|i| *i > mv_cell_start && *i < mv_cell_end);
    assert!(
        dash_pos.is_some(),
        "a dash sits inside the market-value column: {}",
        priv_row.text()
    );
}

// @spec TUI-VIEW-POS-001
#[test]
fn a_symbol_is_ellipsized_only_when_the_clamp_forces_it() {
    let log = vec![
        buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood"),
        buy(
            2,
            18_100,
            "L-L",
            "EXTREMELYLONGTICKER",
            10,
            5_000,
            "Robinhood",
        ),
    ];
    let snap = replay(&log, &[("AMZN", 6_000), ("EXTREMELYLONGTICKER", 6_000)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 6_000, Date(20_000))
        .mark("EXTREMELYLONGTICKER", 6_000, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .estimate(
            "EXTREMELYLONGTICKER",
            common::est("EXTREMELYLONGTICKER", 10_00, 2_00),
        )
        .build();
    let lines = screen_lines(&view, &NavState::new(Screen::Positions));
    let long = lines
        .iter()
        .find(|l| l.text().contains("EXTREMELY"))
        .expect("the long row");
    assert!(
        long.text().contains('…'),
        "over the clamp the symbol ellipsizes: {}",
        long.text()
    );
    assert!(
        !long.text().contains("EXTREMELYLONGTICKER"),
        "the over-clamp symbol does not shear the grid: {}",
        long.text()
    );
    // The rows still share one grid.
    let amzn = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();
    assert_eq!(
        rule_positions(amzn),
        rule_positions(long),
        "the grid holds under the clamp"
    );
}

// @spec TUI-VIEW-LOT-001
#[test]
fn open_lot_columns_size_from_the_data_and_align() {
    let log = vec![
        buy(1, 15_000, "L-A", "AMZN", 100, 800, "Robinhood"),
        buy(2, 19_000, "L-PRIVHLD", "PRIVHLD", 24_150, 1_000, "Schwab"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = ViewBuilder::new(snap).build();
    let lines = screen_lines(&view, &NavState::new(Screen::OpenLots));
    let a = lines
        .iter()
        .find(|l| l.text().contains("L-A "))
        .expect("the AMZN lot");
    let z = lines
        .iter()
        .find(|l| l.text().contains("L-PRIVHLD"))
        .expect("the PRIVHLD lot");
    assert_eq!(
        rule_positions(a),
        rule_positions(z),
        "lot columns share one grid:\n{}\n{}",
        a.text(),
        z.text()
    );
    assert!(
        z.text().contains("24,150"),
        "the share count groups thousands: {}",
        z.text()
    );
    assert_eq!(
        end_of(z, "24,150"),
        end_of(a, "100"),
        "remaining-qty cells right-align in one column:\n{}\n{}",
        a.text(),
        z.text()
    );
}

/// The trailing trend-strip text of a row line: every char from the strip's
/// ramp/dot alphabet at the line's tail.
fn strip_text(line: &ScreenLine) -> String {
    let tail: String = line
        .text()
        .chars()
        .rev()
        .take_while(|c| tui::theme::SPARK_RAMP.contains(c) || *c == tui::theme::SPARK_DOT)
        .collect();
    tail.chars().rev().collect()
}

// @spec TUI-VIEW-POS-001, TUI-VIEW-POS-013
#[test]
fn position_row_trend_strip_carries_direction_tint_a_today_tick_and_dim_dot_padding() {
    use reports::TradingDayKey;
    use tui::testkit::series_point;
    // Two rising captures for AMZN → the strip's captured ramp tints gain with the
    // latest captured cell brightened (the today tick), and the ten
    // not-yet-captured cells pad out the fixed 12-cell strip as dim dots — a
    // timeline filling in, never a two-block stub. (tui-design → "Sparklines")
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 26_126, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .history_point(series_point(
            TradingDayKey(Date(19_998)),
            1_000_00,
            &[("AMZN", 1_000_00)],
            false,
        ))
        .history_point(series_point(
            TradingDayKey(Date(19_999)),
            1_500_00,
            &[("AMZN", 1_500_00)],
            false,
        ))
        .build();
    let lines = screen_lines(&view, &NavState::new(Screen::Positions));
    let amzn = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();

    // The fixed 12-cell strip: 2 captured ramp cells + 10 dot-padded cells.
    let strip = strip_text(amzn);
    assert_eq!(
        strip.chars().count(),
        12,
        "a fixed 12-cell strip: {:?}",
        strip
    );
    assert!(
        strip.ends_with("··········"),
        "10 dim dots pad the young series: {strip}"
    );

    // The latest CAPTURED cell is the brightened today tick in the rising tint.
    let tick = amzn
        .spans
        .iter()
        .find(|s| s.bold && s.text.chars().all(|c| tui::theme::SPARK_RAMP.contains(&c)))
        .expect("a bold ramp tick");
    assert_eq!(
        tick.role,
        tui::theme::Role::Gain,
        "the tick keeps the rising tint"
    );
    // The dot padding is faint chrome — never bold, never the direction tint.
    let dots = amzn.spans.last().expect("spans");
    assert!(
        dots.text.contains('·'),
        "the strip tail is the dot pad: {:?}",
        amzn.spans
    );
    assert_eq!(dots.role, tui::theme::Role::FgFaint, "dots stay dim chrome");
    assert!(!dots.bold, "the dot pad is never brightened");
}

// @spec TUI-VIEW-POS-013
#[test]
fn trend_strip_renders_honest_states_under_two_points_and_windows_to_twelve() {
    use reports::TradingDayKey;
    use tui::testkit::series_point;
    let mk = |points: &[(i32, i64)]| {
        let log = vec![buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood")];
        let snap = replay(&log, &[("AMZN", 26_126)]);
        let mut b = ViewBuilder::new(snap)
            .mark("AMZN", 26_126, Date(20_000))
            .estimate("AMZN", common::est("AMZN", 10_00, 2_00));
        for (d, v) in points {
            b = b.history_point(series_point(
                TradingDayKey(Date(*d)),
                *v,
                &[("AMZN", *v)],
                false,
            ));
        }
        screen_lines(&b.build(), &NavState::new(Screen::Positions))
    };
    let amzn_strip = |lines: &Vec<ScreenLine>| {
        strip_text(lines.iter().find(|l| l.text().contains("AMZN")).unwrap())
    };

    // Zero captures: the full 12-dot strip — a deliberate not-yet-begun timeline,
    // not a missing column.
    let lines = mk(&[]);
    assert_eq!(
        amzn_strip(&lines),
        "············",
        "zero captures → all dots"
    );
    let amzn = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();
    assert!(
        !amzn.spans.iter().any(|s| s.bold && s.text.contains('·')),
        "no today tick when nothing is captured"
    );

    // One capture: a single MID-RUNG tick (no trend — not a bottom-scraping ▁)
    // plus eleven dots; the lone tick is still the brightened today cell.
    let lines = mk(&[(19_999, 1_000_00)]);
    assert_eq!(
        amzn_strip(&lines),
        "▄···········",
        "one capture → a mid-rung tick + 11 dots"
    );
    let amzn = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();
    let tick = amzn
        .spans
        .iter()
        .find(|s| s.bold && s.text == "▄")
        .expect("the lone tick is bold");
    assert_eq!(
        tick.role,
        tui::theme::Role::Flat,
        "a single point has no trend — flat tint"
    );

    // Fourteen captures: the strip windows to the most recent TWELVE — full
    // width, no dots, the oldest two days dropped.
    let pts: Vec<(i32, i64)> = (0..14)
        .map(|i| (19_986 + i, 1_000_00 + (i as i64) * 10_00))
        .collect();
    let lines = mk(&pts);
    let strip = amzn_strip(&lines);
    assert_eq!(strip.chars().count(), 12, "windowed to 12: {strip}");
    assert!(
        !strip.contains('·'),
        "a full window has no dot padding: {strip}"
    );
}

// @spec TUI-VIEW-POS-013
#[test]
fn platform_group_rows_render_no_trend_strip() {
    use reports::TradingDayKey;
    use tui::testkit::series_point;
    // A platform group row has no single per-symbol series — it renders no strip
    // at all (the day-change dash rationale), not a dot strip implying a series
    // that will fill in.
    let log = vec![buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood")];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 26_126, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .history_point(series_point(
            TradingDayKey(Date(19_999)),
            1_000_00,
            &[("AMZN", 1_000_00)],
            false,
        ))
        .build();
    let mut nav = NavState::new(Screen::Positions);
    nav.grouping = tui::views::Grouping::ByPlatform;
    let lines = screen_lines(&view, &nav);
    let row = lines
        .iter()
        .find(|l| l.text().contains("Robinhood"))
        .unwrap();
    assert_eq!(
        strip_text(row),
        "",
        "no strip on a platform group row: {}",
        row.text()
    );
}

// @spec TUI-VIEW-POS-004, TUI-VIEW-POS-010
#[test]
fn basis_and_gain_pct_columns_size_from_the_data_and_align() {
    // The private-holding fixture carries a six-figure basis ($241,500.00 for
    // 24,150 sh @ $10) beside the modest PENNY basis — the new columns size from
    // the data and never shear the shared grid.
    let view = private_holding_view();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);
    let row = |sym: &str| {
        lines
            .iter()
            .find(|l| l.text().contains(sym))
            .unwrap_or_else(|| panic!("a row for {sym}"))
    };
    let amzn = row("AMZN");
    let penny = row("PENNY");
    let priv_row = row("PRIVHLD");

    // The wide basis renders compact — basis survives degradation (PRIVHLD is
    // unpriced) — and right-aligns with the other basis cells.
    assert!(
        priv_row.text().contains("$241.5k"),
        "the six-figure basis compacts: {}",
        priv_row.text()
    );
    assert_eq!(
        end_of(priv_row, "$241.5k"),
        end_of(amzn, "$31.0k"),
        "basis cells right-align in one column:\n{}\n{}",
        priv_row.text(),
        amzn.text()
    );

    // The gain-% cells render on the priced rows (PENNY: $800 over a $2,400
    // basis → 33.3%); the grid stays true across all three rows with the new
    // columns in place.
    assert!(
        penny.text().contains("33.3%"),
        "PENNY gain %: {}",
        penny.text()
    );
    assert!(
        amzn.text().contains("422.5%"),
        "AMZN gain %: {}",
        amzn.text()
    );
    let amzn_rules = rule_positions(amzn);
    assert_eq!(
        rule_positions(penny),
        amzn_rules,
        "PENNY aligns:\n{}\n{}",
        amzn.text(),
        penny.text()
    );
    assert_eq!(
        rule_positions(priv_row),
        amzn_rules,
        "PRIVHLD aligns:\n{}\n{}",
        amzn.text(),
        priv_row.text()
    );
}

// ===========================================================================
// Column title rows (TUI-VIEW-POS-007, TUI-VIEW-LOT-002): tracked-caps faint
// titles on the SAME data-driven grid as the rows — pinned against the same
// private-holding extremes as the data alignment above. And the company-name
// column (TUI-VIEW-POS-009, TUI-VIEW-LOT-003).
// ===========================================================================

// @spec TUI-VIEW-POS-007
#[test]
fn positions_title_row_is_faint_tracked_caps_and_aligns_with_its_columns() {
    let view = private_holding_view();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);

    let title = lines
        .iter()
        .find(|l| l.text().contains("SYMBOL") && l.text().contains("SHARES"))
        .expect("the Positions title row");
    for t in [
        "SYMBOL", "NAME", "SHARES", "PRICE", "VALUE", "BASIS", "UNREAL", "GAIN%", "NET", "DAY",
        "SHARE",
    ] {
        assert!(
            title.text().contains(t),
            "the {t} title renders: {}",
            title.text()
        );
    }
    // Tracked-caps faint: every titled segment carries the fg-faint role.
    assert_eq!(title.span_role("SYMBOL"), Some(tui::theme::Role::FgFaint));
    assert_eq!(title.span_role("GAIN%"), Some(tui::theme::Role::FgFaint));

    // The title row rides the SAME grid: its │ rules sit at the data rows'
    // positions, across the extreme fixture rows.
    let amzn = lines.iter().find(|l| l.text().contains("AMZN")).unwrap();
    let priv_row = lines.iter().find(|l| l.text().contains("PRIVHLD")).unwrap();
    assert_eq!(
        rule_positions(title),
        rule_positions(amzn),
        "title aligns:\n{}\n{}",
        title.text(),
        amzn.text()
    );
    assert_eq!(
        rule_positions(title),
        rule_positions(priv_row),
        "title aligns:\n{}\n{}",
        title.text(),
        priv_row.text()
    );

    // The title sits directly above the data rows (after the double-entry rule).
    let rule_idx = lines
        .iter()
        .position(|l| l.text().starts_with('\u{2550}'))
        .unwrap();
    let title_idx = lines
        .iter()
        .position(|l| l.text().contains("SYMBOL"))
        .unwrap();
    let row_idx = lines
        .iter()
        .position(|l| l.text().contains("AMZN"))
        .unwrap();
    assert!(
        rule_idx < title_idx && title_idx < row_idx,
        "band ═ title ═ rows order"
    );
}

// @spec TUI-VIEW-LOT-002
#[test]
fn open_lots_title_row_aligns_with_its_columns() {
    let log = vec![
        buy(1, 15_000, "L-A", "AMZN", 100, 800, "Robinhood"),
        buy(2, 19_000, "L-PRIVHLD", "PRIVHLD", 24_150, 1_000, "Schwab"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let view = ViewBuilder::new(snap).build();
    let lines = screen_lines(&view, &NavState::new(Screen::OpenLots));
    let title = lines
        .iter()
        .find(|l| l.text().contains("LOT") && l.text().contains("BASIS"))
        .expect("the Open Lots title row");
    for t in [
        "LOT", "SYMBOL", "NAME", "DATE", "SOURCE", "TERM", "REM", "BASIS", "$/SH", "PLATFORM",
        "CODE",
    ] {
        assert!(
            title.text().contains(t),
            "the {t} title renders: {}",
            title.text()
        );
    }
    assert_eq!(title.span_role("PLATFORM"), Some(tui::theme::Role::FgFaint));
    let a = lines.iter().find(|l| l.text().contains("L-A ")).unwrap();
    let z = lines
        .iter()
        .find(|l| l.text().contains("L-PRIVHLD"))
        .unwrap();
    assert_eq!(
        rule_positions(title),
        rule_positions(a),
        "title aligns:\n{}\n{}",
        title.text(),
        a.text()
    );
    assert_eq!(
        rule_positions(title),
        rule_positions(z),
        "title aligns:\n{}\n{}",
        title.text(),
        z.text()
    );
}

// @spec TUI-VIEW-POS-009
#[test]
fn positions_render_the_company_name_with_ticker_fallback_and_clamped_ellipsis() {
    let log = vec![
        buy(1, 18_000, "L-A", "AMZN", 10, 5_000, "Robinhood"),
        buy(2, 18_100, "L-Q", "QUBT", 10, 5_000, "Robinhood"),
        buy(3, 18_200, "L-P", "PLTR", 10, 5_000, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126), ("QUBT", 1_000), ("PLTR", 2_000)]);
    let view = ViewBuilder::new(snap)
        .mark("AMZN", 26_126, Date(20_000))
        .mark("QUBT", 1_000, Date(20_000))
        .mark("PLTR", 2_000, Date(20_000))
        .estimate("AMZN", common::est("AMZN", 10_00, 2_00))
        .estimate("QUBT", common::est("QUBT", 10_00, 2_00))
        .estimate("PLTR", common::est("PLTR", 10_00, 2_00))
        .display_name("AMZN", "Amazon.com")
        .display_name("QUBT", "Quantum Computing Incorporated Of Earth")
        .build();
    let nav = NavState::new(Screen::Positions);
    let lines = screen_lines(&view, &nav);
    let row = |sym: &str| lines.iter().find(|l| l.text().contains(sym)).unwrap();

    // A mapped symbol shows its company name beside the ticker.
    assert!(
        row("AMZN").text().contains("Amazon.com"),
        "{}",
        row("AMZN").text()
    );
    // An over-clamp name is truncated with … — and never shears the grid.
    let qubt = row("QUBT");
    assert!(
        qubt.text().contains('…'),
        "the long name ellipsizes: {}",
        qubt.text()
    );
    assert!(
        !qubt.text().contains("Incorporated"),
        "clamped: {}",
        qubt.text()
    );
    assert_eq!(
        rule_positions(row("AMZN")),
        rule_positions(qubt),
        "the grid holds"
    );
    // An unmapped symbol renders the ticker in the name column: the PLTR row
    // carries the ticker twice (symbol + name cells).
    let pltr_text = row("PLTR").text();
    assert!(
        pltr_text.matches("PLTR").count() >= 2,
        "ticker fallback: {pltr_text}"
    );
}

// @spec TUI-VIEW-LOT-003
#[test]
fn open_lots_render_the_company_name_with_ticker_fallback() {
    let log = vec![
        buy(1, 15_000, "L-A", "AMZN", 100, 800, "Robinhood"),
        buy(2, 19_000, "L-P", "PLTR", 10, 5_000, "Schwab"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126), ("PLTR", 2_000)]);
    let view = ViewBuilder::new(snap)
        .display_name("AMZN", "Amazon.com")
        .build();
    let lines = screen_lines(&view, &NavState::new(Screen::OpenLots));
    let a = lines.iter().find(|l| l.text().contains("L-A ")).unwrap();
    assert!(
        a.text().contains("Amazon.com"),
        "the lot row names the company: {}",
        a.text()
    );
    let p = lines.iter().find(|l| l.text().contains("L-P ")).unwrap();
    assert!(
        p.text().matches("PLTR").count() >= 2,
        "ticker fallback: {}",
        p.text()
    );
    assert_eq!(
        rule_positions(a),
        rule_positions(p),
        "the grid holds with the name column"
    );
}
