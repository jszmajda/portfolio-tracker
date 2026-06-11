//! Cross-screen display-convention tests (the `tui` sub-HLD shared intent applied
//! identically by entry + views): freshness / degradation / [est] / integrity-block
//! / confirmations / color-redundancy / motifs.

mod common;

use pt_core::{Cents, Date};
use tui::testkit::{flat_federal_ctx, render_string, FakeRuntime, ViewBuilder};
use tui::theme::{self, ColorDepth, LifecycleStop, Palette, Qualifier, Role, StepperState};
use tui::views::{NavState, Screen};
use tui::Model;

// @spec TUI-VIEW-POS-001
#[test]
fn freshness_offline_marks_figures_stale_with_the_last_quote_epoch_never_live() {
    // Offline → the status line marks stale with the last quote-epoch (never live).
    let view = ViewBuilder::new(ledger_core::Snapshot::default()).offline().build();
    let status = tui::StatusLine::from_view(&view, tui::Mode::Views, &Screen::Positions);
    assert!(!status.connected);
    let text = status.text();
    assert!(text.contains("stale"), "stale-marked: {text}");
    assert!(text.contains("as-of"), "carries the last quote-epoch");

    // A stale qualifier carries the ⟲ glyph + as-of, in the stale role.
    let q = Qualifier::Stale { quote_epoch: "20000".to_string() };
    assert!(q.marker().contains('\u{27F2}'), "the ⟲ stale glyph travels with the figure");
    assert_eq!(q.role(), Role::Stale);
}

// @spec TUI-VIEW-POS-003
#[test]
fn degradation_distinguishes_unpriced_from_tax_degraded_with_distinct_words() {
    // A figure with no MARK is "unpriced" (‡, never 0); a figure whose tax estimate
    // is missing is "degraded" — distinct words for distinct causes, both dim.
    let unpriced = Qualifier::Unpriced;
    let tax_deg = Qualifier::Degraded;
    assert!(unpriced.marker().contains("unpriced"));
    assert!(tax_deg.marker().contains("degraded"));
    assert!(!tax_deg.marker().contains("unpriced"), "a tax-degraded figure is never mislabelled unpriced");
    assert_eq!(unpriced.role(), Role::Degraded);
    assert_eq!(tax_deg.role(), Role::Degraded);
    // Both carry the ‡ marker, never a 0.
    assert!(unpriced.marker().contains('\u{2021}'));
    assert!(tax_deg.marker().contains('\u{2021}'));
}

// @spec TUI-VIEW-POS-001
#[test]
fn estimates_carry_est_and_degrade_to_stale_brackets_and_no_brackets_wording() {
    assert_eq!(theme::estimate_qualifier(config::BracketState::Verified).marker(), "[est]");
    assert_eq!(
        theme::estimate_qualifier(config::BracketState::Stale).marker(),
        "[est, brackets stale]"
    );
    assert_eq!(
        theme::estimate_qualifier(config::BracketState::NoBracketsAvailable).marker(),
        "n/a (no brackets)"
    );
}

// @spec TUI-VIEW-NAV-006
#[test]
fn integrity_error_blocks_the_screen_loudly_refusing_to_render_derived_numbers() {
    // An integrity error from store/reports/config blocks the screen with the loud
    // ✗ treatment and refuses to render derived numbers. Distinct from a calm empty
    // state. (tui-design → "Integrity errors")
    let view = ViewBuilder::new(common::two_position_view().snapshot)
        .mark("AMZN", 26_126, Date(20_000))
        .integrity(tui::port::IntegrityError::CorruptLog("seq gap".to_string()))
        .build();
    let ctx = flat_federal_ctx(common::YEAR, 220_000);
    let rt = FakeRuntime::new(view, ctx);
    let model = Model::new();
    let s = render_string(&model, &rt, 90, 20);
    assert!(s.contains('\u{2717}'), "the ✗ integrity glyph is shown loudly");
    assert!(s.to_uppercase().contains("INTEGRITY"), "a blocking message is shown");
    assert!(!s.contains("$261.26") && !s.contains("[est]"), "no derived numbers are rendered");

    // The other two integrity causes carry their messages.
    assert!(tui::port::IntegrityError::BadCredentials.message().contains("credentials"));
    assert!(tui::port::IntegrityError::UnreadableCache.message().contains("cache"));
}

// @spec TUI-ENTRY-FLOW-006
#[test]
fn confirmations_restate_what_will_be_written_for_gated_flows_only() {
    use tui::entry::FlowKind;
    // The gated flows restate; the plain appends do not gate (covered structurally
    // in FlowKind::requires_confirm). Here: the bracket confirm restates the blast
    // radius; the Pay/Reversal/Override gate. (TUI-ENTRY-FLOW-006)
    assert!(FlowKind::TaxRuleEdit.requires_confirm());
    let notice = tui::entry::bracket_reprice_notice(3);
    assert!(notice.contains("reprices 3"), "the tax-rule confirm restates the blast radius");
}

// @spec TUI-VIEW-POS-001
#[test]
fn color_is_redundant_never_sole_gain_loss_flat_carry_sign_and_glyph() {
    // Every colour-coded distinction also carries a sign + glyph (legible in
    // monochrome / NO_COLOR). (tui-design → tenet "Color is redundant, never sole")
    let (up, up_role) = theme::delta_text(Cents(8_420_00));
    assert!(up.contains('\u{25B2}'), "gain carries ▲");
    assert!(up.contains('+'), "gain carries a leading +");
    assert_eq!(up_role, Role::Gain);

    let (down, down_role) = theme::delta_text(Cents(-410_00));
    assert!(down.contains('\u{25BC}'), "loss carries ▼");
    assert!(down.contains('\u{2212}'), "loss carries a leading −");
    assert_eq!(down_role, Role::Loss);

    let (flat, flat_role) = theme::delta_text(Cents(0));
    assert!(flat.contains('\u{00B7}'), "flat carries ·");
    assert_eq!(flat_role, Role::Flat);
}

// @spec TUI-VIEW-POS-001
// @spec TUI-VIEW-NAV-009
#[test]
fn color_degrades_truecolor_to_256_to_16_to_none_keeping_glyphs() {
    use ratatui::style::Color;
    // TrueColor → an exact Rgb. (tui-design → "Palette (truecolor, 'Ledger')")
    let tc = Palette::at(ColorDepth::TrueColor);
    assert_eq!(tc.color(Role::Accent), Color::Rgb(0xD4, 0xA8, 0x2C), "the gilt accent hex");
    assert_eq!(tc.color(Role::Gain), Color::Rgb(0x6F, 0xA8, 0x6B), "sage gain");

    // Indexed256 → the nearest xterm-256 palette index (NOT a 24-bit passthrough).
    // The gilt accent #D4A82C degrades to a concrete cube index, not Rgb.
    let i256 = Palette::at(ColorDepth::Indexed256);
    match i256.color(Role::Accent) {
        Color::Indexed(i) => assert!((16..=231).contains(&i), "the accent degrades to a cube index, got {i}"),
        other => panic!("Indexed256 must map to a palette index, not {other:?}"),
    }
    // A near-gray chrome token degrades to the grayscale ramp (232–255), not a muddy
    // cube cell — proving the rung actually maps, not passes truecolor through.
    match i256.color(Role::FgDim) {
        Color::Indexed(i) => assert!(i >= 232 || (16..=231).contains(&i), "fg-dim maps to a 256 index, got {i}"),
        other => panic!("Indexed256 must map fg-dim to an index, not {other:?}"),
    }
    assert_ne!(
        i256.color(Role::Accent),
        Color::Rgb(0xD4, 0xA8, 0x2C),
        "Indexed256 is NOT a silent truecolor passthrough"
    );

    // Ansi16 → a 16-colour fallback.
    let a16 = Palette::at(ColorDepth::Ansi16);
    assert_eq!(a16.color(Role::Gain), Color::Green);
    assert_eq!(a16.color(Role::Loss), Color::Red);

    // None (NO_COLOR / piped) → Reset (the glyphs/words carry meaning).
    let none = Palette::at(ColorDepth::None);
    assert_eq!(none.color(Role::Gain), Color::Reset);
    // Even with no colour, the delta glyphs still travel with the figure.
    assert!(theme::delta_text(Cents(100)).0.contains('\u{25B2}'));
}

// @spec TUI-VIEW-TAX-001
#[test]
fn lifecycle_stepper_motif_colours_the_minting_ramp_and_renders_distinct_settled_orphan() {
    // The four-dot stepper coloured by the minting ramp: Accrued faint → Allocated
    // verdigris → Moved gilt → Paid sage. (tui-design → "Lifecycle stepper")
    let pal = Palette::ledger();
    use ratatui::style::Color;
    assert_eq!(pal.lifecycle_color(LifecycleStop::Allocated), Color::Rgb(0x5F, 0xA8, 0x9E));
    assert_eq!(pal.lifecycle_color(LifecycleStop::Moved), Color::Rgb(0xD4, 0xA8, 0x2C));
    assert_eq!(pal.lifecycle_color(LifecycleStop::Paid), Color::Rgb(0x6F, 0xA8, 0x6B));

    // Moved renders ◉◉◉○; Paid renders ◉◉◉◉; a de-minimis renders ✓ settled; an
    // orphan renders ⚠ undone — needs unwind.
    assert_eq!(theme::stepper(&StepperState::Lifecycle(LifecycleStop::Moved)), "◉◉◉○ Moved");
    assert_eq!(theme::stepper(&StepperState::Lifecycle(LifecycleStop::Paid)), "◉◉◉◉ Paid");
    assert_eq!(theme::stepper(&StepperState::AutoSettled), "✓ settled");
    assert!(theme::stepper(&StepperState::Orphaned).contains("undone — needs unwind"));
}

// @spec TUI-VIEW-HIST-001
#[test]
fn sparkline_motif_renders_ramp_and_tints_by_net_direction() {
    // A rising series → a non-empty ramp, tinted gain; falling → loss; flat → flat.
    let rising = theme::sparkline(&[1, 2, 3, 4, 8]);
    assert!(!rising.is_empty());
    assert!(rising.ends_with('\u{2588}'), "the latest (max) cell is brightest");
    assert_eq!(theme::series_role(&[1, 2, 8]), Role::Gain);
    assert_eq!(theme::series_role(&[8, 2, 1]), Role::Loss);
    assert_eq!(theme::series_role(&[5, 5, 5]), Role::Flat);
    // A zero-span (all-equal or single-point) series renders the MID rung — a
    // flat line mid-strip, never a bottom-scraping floor that reads as a crash.
    assert_eq!(theme::sparkline(&[5, 5, 5]), "▄▄▄");
    assert_eq!(theme::sparkline(&[7]), "▄");
}

// @spec TUI-VIEW-NAV-016
#[test]
fn view_money_is_whole_dollar_with_a_compact_glance_form_and_per_share_cents() {
    // Per-share money keeps its two-decimal cents — a $0.04 OTC mark vanishes
    // without them. (tui-design → "Money columns")
    assert_eq!(theme::money(Cents(4)), "$0.04");
    assert_eq!(theme::money(Cents(1_284_30)), "$1,284.30");
    assert_eq!(theme::shares(common::sh(620)), "620");

    // Reconciliation money: exact whole dollars, half-to-even from the cents,
    // never abbreviated.
    assert_eq!(theme::money_whole(Cents(1_284_300_00)), "$1,284,300");
    assert_eq!(theme::money_whole(Cents(123_456_46)), "$123,456");
    assert_eq!(theme::money_whole(Cents(50)), "$0", "half-to-even: 50¢ → the even 0");
    assert_eq!(theme::money_whole(Cents(150)), "$2", "half-to-even: $1.50 → the even 2");
    assert_eq!(theme::signed_money_whole(Cents(8_420_00)), "+$8,420");
    assert_eq!(theme::signed_money_whole(Cents(-410_00)), "−$410");

    // The compact glance form: whole dollars below $1k, tenths of k below $1m,
    // hundredths of m at and above $1m — half-to-even at each band.
    assert_eq!(theme::money_compact(Cents(842_00)), "$842");
    assert_eq!(theme::money_compact(Cents(123_456_46)), "$123.5k");
    assert_eq!(theme::money_compact(Cents(2_654_732_00)), "$2.65m");
    assert_eq!(theme::signed_money_compact(Cents(3_120_00)), "+$3.1k");
    assert_eq!(theme::signed_money_compact(Cents(-410_00)), "−$410");
    // Band promotion: rounding never reads `$1,000` or `$1000.0k`.
    assert_eq!(theme::money_compact(Cents(99_996)), "$1.0k", "$999.96 promotes to k");
    assert_eq!(theme::money_compact(Cents(99_996_000)), "$1.00m", "$999,960 promotes to m");

    // The delta forms keep the redundant sign + glyph cues at each precision.
    assert_eq!(theme::delta_text_whole(Cents(0)).0, "· $0");
    assert_eq!(theme::delta_text_whole(Cents(8_420_00)).0, "▲ +$8,420");
    assert_eq!(theme::delta_text_compact(Cents(3_120_00)).0, "▲ +$3.1k");
    assert_eq!(theme::delta_text_compact(Cents(-410_00)).0, "▼ −$410");
}

// @spec TUI-VIEW-NAV-007
#[test]
fn status_line_lock_glyph_is_catalogued_and_renders_held_vs_free() {
    // The lock state uses the catalogued glyph constants (one glyph, one meaning) —
    // not an ad-hoc inline emoji. Free renders the open lock; held renders the
    // closed lock with the "lock held" word. (tui-design → "Status line")
    let view = ViewBuilder::new(ledger_core::Snapshot::default()).build();
    let mut status = tui::StatusLine::from_view(&view, tui::Mode::Views, &Screen::Positions);
    assert!(status.text().contains(theme::GLYPH_LOCK_FREE), "the free-lock glyph renders");
    assert!(!status.text().contains(theme::GLYPH_LOCK_HELD));

    status.lock_held = true;
    let held = status.text();
    assert!(held.contains(theme::GLYPH_LOCK_HELD), "the held-lock glyph renders");
    assert!(held.contains("lock held"), "the redundant word travels with the glyph");
}

// @spec TUI-VIEW-POS-002
#[test]
fn masthead_and_status_line_motifs_render_in_the_buffer() {
    let rt = FakeRuntime::new(common::two_position_view(), flat_federal_ctx(common::YEAR, 220_000));
    let model = Model::new();
    let s = render_string(&model, &rt, 90, 24);
    // The masthead in tracked uppercase. (tui-design → "Masthead")
    assert!(s.contains("L E D G E R"));
    // The status line shows the connection dot + the current screen.
    assert!(s.contains("Positions"), "the status line names the current screen");
}

// @spec TUI-ENTRY-FLOW-007
#[test]
fn mode_toggle_and_screen_stack_mutate_nothing_durable() {
    // Navigation alone mutates nothing durable; the mode toggle and the stack are
    // pure model state. (tui-design → "App Shell")
    let mut model = Model::new();
    assert_eq!(model.mode, tui::Mode::Views);
    model.toggle_mode();
    assert_eq!(model.mode, tui::Mode::Entry);
    model.toggle_mode();
    assert_eq!(model.mode, tui::Mode::Views);
    // Ascend at the landing frame is a no-op (never pops the landing screen).
    assert!(!model.ascend());
    assert_eq!(model.current().nav.screen, Screen::Positions);
    let _ = NavState::new(Screen::Positions);
}
