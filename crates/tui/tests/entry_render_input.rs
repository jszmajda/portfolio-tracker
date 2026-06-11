//! Entry-shell RENDER (`TUI-ENTRY-FLOW-009`), lot-picker render (`TUI-ENTRY-LOT-006`),
//! and INPUT/keybinding layer (`TUI-ENTRY-FLOW-008`) tests — driven against the
//! ratatui `TestBackend` (via `render_string`) and the Model's pure update methods.

mod common;

use pt_core::{Cents, Date, MicroShares};
use tui::entry::{
    BuyForm, FlowKind, GainTaxPreview, InlineError, LotPicker, Phase, RetryReason, SellForm,
    SplitForm, VestForm,
};
use tui::form::{Field, FormAction, FormModel, InputMode};
use tui::testkit::{buy, flat_federal_ctx, render_string, replay, FakeRuntime, ViewBuilder};
use tui::{EntryContext, Mode, Model};

fn ms(n: i64) -> MicroShares {
    MicroShares(n * pt_core::SHARE_SCALE)
}

fn empty_runtime() -> FakeRuntime {
    let snap = ledger_core::Snapshot::default();
    FakeRuntime::new(
        ViewBuilder::new(snap).build(),
        flat_federal_ctx(common::YEAR, 220_000),
    )
}

fn a_buy() -> BuyForm {
    BuyForm {
        lot_id: "L1".to_string(),
        symbol: "AMZN".to_string(),
        qty: ms(10),
        unit_price: Cents(10_000),
        fees: Cents(0),
        date: Date(20_000),
        platform: "Robinhood".to_string(),
        tracking_code: None,
        confirmed_new_symbol: true,
    }
}

/// Two AMZN lots on Robinhood (older 100, newer 100). Returns the snapshot.
fn multi_lot_snapshot() -> ledger_core::Snapshot {
    let log = vec![
        buy(1, 15_000, "old", "AMZN", 100, 800, "Robinhood"),
        buy(2, 19_000, "new", "AMZN", 100, 17_200, "Robinhood"),
    ];
    replay(&log, &[("AMZN", 26_126)])
}

fn open_buy_model() -> Model {
    let mut model = Model::new();
    model.open_entry(EntryContext::form(FormModel::for_buy(&a_buy())));
    model
}

// ===========================================================================
// TUI-ENTRY-FLOW-009 — form rendering.
// ===========================================================================

// @spec TUI-ENTRY-FLOW-009
#[test]
fn entry_form_renders_rounded_titled_panel_with_caret_caps_labels_and_footer() {
    // The active Buy form renders as a rounded titled panel; the focused field
    // carries the gilt focus caret (▎); labels render tracked-caps; the footer is a
    // hint line of the active bracketed accelerators. (TUI-ENTRY-FLOW-009)
    let model = open_buy_model();
    let rt = empty_runtime();
    let s = render_string(&model, &rt, 90, 24);

    // The rounded panel border + the flow title.
    assert!(
        s.contains('\u{256D}'),
        "the rounded panel top-left corner ╭ is drawn"
    );
    assert!(s.contains("Buy"), "the panel is titled by the flow");

    // Tracked-caps labels beside the inputs.
    assert!(
        s.contains("SYMBOL"),
        "the symbol label renders tracked-caps"
    );
    assert!(
        s.contains("UNIT PRICE"),
        "the unit-price label renders tracked-caps"
    );

    // The gilt focus caret sits on the focused (first) field.
    assert!(
        s.contains('\u{258E}'),
        "the ▎ focus caret renders on the focused field"
    );

    // The footer hint line shows the active bracketed accelerators.
    assert!(
        s.contains("[enter] submit"),
        "the footer hints the submit accelerator"
    );
    assert!(
        s.contains("[esc] cancel"),
        "the footer hints the cancel accelerator"
    );
}

// @spec TUI-ENTRY-FLOW-009
// @spec TUI-ENTRY-FLOW-001
#[test]
fn inline_validation_error_renders_in_the_one_slot_beneath_the_offending_field() {
    // A FLOW-001 inline validation rejection writes into the single inline slot,
    // directly beneath its field, in the error role (the ✗ glyph travels). The slot
    // is shared with the submit-time disagreement. (TUI-ENTRY-FLOW-009/001)
    let mut model = open_buy_model();
    let err = InlineError::Ledger(ledger_core::LedgerError::NonPositiveQty);
    // The Qty field is index 1 in the Buy form.
    model.entry_top_mut().unwrap().form.set_error(1, &err);

    let rt = empty_runtime();
    let s = render_string(&model, &rt, 90, 24);
    assert!(
        s.contains("NonPositiveQty"),
        "the verified kernel error's words render inline: {s}"
    );
    assert!(
        s.contains('\u{2717}'),
        "the ✗ error glyph travels with an error-role rejection"
    );
}

// @spec TUI-ENTRY-FLOW-009
// @spec TUI-ENTRY-FLOW-002
#[test]
fn submit_disagreement_reuses_the_same_inline_slot_as_inline_validation() {
    // A FLOW-002 submit-time disagreement re-renders in the SAME single inline slot
    // (inline never overrides submit). Both an inline validation error and a submit
    // rejection write through FormModel::set_error onto the same field-attached slot.
    // (TUI-ENTRY-FLOW-009/002)
    let mut form = FormModel::for_buy(&a_buy());
    // First an inline advisory error on Qty.
    form.set_error(
        1,
        &InlineError::Ledger(ledger_core::LedgerError::NonPositiveQty),
    );
    let first_field = form.error.as_ref().unwrap().field;
    // Then a submit-time disagreement on the same field overwrites the one slot.
    form.set_error(
        1,
        &InlineError::Ledger(ledger_core::LedgerError::InsufficientShares),
    );
    assert_eq!(
        form.error.as_ref().unwrap().field,
        first_field,
        "the same single slot"
    );
    assert!(
        form.error
            .as_ref()
            .unwrap()
            .message
            .contains("InsufficientShares"),
        "submit re-renders into the one slot — inline never overrides submit"
    );
    // Only one slot exists structurally (an Option, not a per-field vec).
    assert!(form.error.is_some());
}

// @spec TUI-ENTRY-FLOW-009
#[test]
fn footer_accelerators_track_the_phase_retry_confirm_and_new_symbol() {
    // The footer accelerators live in the CURRENT phase/flow. A returned-control
    // retry shows [r]; a new-symbol guard shows [c]; a gated confirm shows [enter]
    // confirm. (TUI-ENTRY-FLOW-009)
    let mut form = FormModel::for_buy(&a_buy());

    form.set_phase(Phase::Retry(RetryReason::Unreachable));
    assert!(
        form.footer_hint().contains("[r] retry"),
        "a returned-control submit offers [r]etry"
    );

    form.set_phase(Phase::ConfirmNewSymbol("new symbol".to_string()));
    assert!(
        form.footer_hint().contains("[c] confirm"),
        "the new-symbol guard offers [c]onfirm"
    );

    form.set_phase(Phase::Confirming);
    assert!(
        form.footer_hint().contains("[enter] confirm"),
        "a gated flow offers [enter] confirm"
    );

    // A Sell offers [F] FIFO in the editing phase.
    let sell = FormModel::new(FlowKind::Sell, vec![Field::new("Qty", "0")]);
    assert!(
        sell.footer_hint().contains("[F] FIFO"),
        "the Sell/lot-picker offers FIFO-fill"
    );
}

// @spec TUI-ENTRY-FLOW-009
#[test]
fn every_flow_kind_renders_a_titled_panel_chrome() {
    // Every FlowKind renders through the one form-layout path with its own title +
    // panel chrome + footer hint. This pins the CHROME for all twelve (title, rounded
    // panel, footer) — not the per-flow FIELD layout, which is asserted separately for
    // the flows that have a real field builder (see the next test). (TUI-ENTRY-FLOW-009)
    let kinds = [
        FlowKind::Buy,
        FlowKind::Vest,
        FlowKind::Sell,
        FlowKind::Split,
        FlowKind::Reversal,
        FlowKind::Allocate,
        FlowKind::Move,
        FlowKind::Pay,
        FlowKind::Override,
        FlowKind::ResidencyEdit,
        FlowKind::TaxRuleEdit,
        FlowKind::PlatformAliasEdit,
    ];
    let rt = empty_runtime();
    for kind in kinds {
        let title = tui::form::flow_title(kind);
        assert!(!title.is_empty(), "{kind:?} has a render title");
        // Render a single-field form for the kind and assert the title + panel chrome.
        let mut model = Model::new();
        model.open_entry(EntryContext::form(FormModel::new(
            kind,
            vec![Field::new("Field", "x")],
        )));
        let s = render_string(&model, &rt, 90, 16);
        assert!(
            s.contains(&title),
            "{kind:?} renders its titled panel: {title}"
        );
        assert!(s.contains('\u{256D}'), "{kind:?} renders the rounded panel");
        assert!(
            s.contains("[esc] cancel"),
            "{kind:?} renders the footer hint"
        );
    }
}

// @spec TUI-ENTRY-FLOW-009
// @spec TUI-ENTRY-ACT-001, TUI-ENTRY-ACT-002, TUI-ENTRY-ACT-003, TUI-ENTRY-ACT-004
#[test]
fn the_flows_with_field_builders_render_their_per_flow_labels_through_the_one_path() {
    // For each flow that has a real `FormModel::for_*` field builder, the FLOW-009
    // FIELD layout is realized: the per-flow tracked-caps labels render through the
    // one form-layout path (not a synthetic single field). The seven Tax/Reversal/
    // Config flows compose through free functions (`compose_allocate`/`compose_move`/
    // …) and do not yet have field builders — FLOW-009's field layout is realized for
    // the five below; the chrome-only test above covers the rest. (TUI-ENTRY-FLOW-009)
    let rt = empty_runtime();
    let platforms = config::PlatformList::new(vec!["Robinhood".to_string()]);
    let aliases = config::AliasMap::new(std::collections::BTreeMap::new());
    let residency = config::ResidencyTimeline::from_entries(vec![config::ResidencyEntry {
        effective_date: Date(0),
        state_code: "DC".to_string(),
    }])
    .unwrap();

    // Buy: Symbol · Qty · Unit price · Date · Fees · Platform · Tracking.
    let buy_form = BuyForm::with_defaults(&rt, "L1".to_string(), "AMZN", &platforms, &aliases);
    assert_labels_render(
        &rt,
        FormModel::for_buy(&buy_form),
        &["SYMBOL", "QTY", "UNIT PRICE", "FEES"],
    );

    // Vest: FMV/share, no Fees.
    let vest_form = VestForm::with_defaults(&rt, "V1".to_string(), "AMZN", &platforms, &aliases);
    assert_labels_render(
        &rt,
        FormModel::for_vest(&vest_form),
        &["SYMBOL", "QTY", "FMV/SHARE", "PLATFORM"],
    );

    // Split: Symbol · Ratio · Date.
    let split_form = SplitForm {
        symbol: "AMZN".to_string(),
        ratio_num: 2,
        ratio_den: 1,
        date: Date(20_000),
    };
    assert_labels_render(
        &rt,
        FormModel::for_split(&split_form),
        &["SYMBOL", "RATIO", "DATE"],
    );

    // Sell: carries the accrues-to-state label.
    let sell_form = SellForm::with_defaults(
        &rt,
        "S1".to_string(),
        "AMZN",
        &residency,
        &platforms,
        &aliases,
    );
    assert_labels_render(
        &rt,
        FormModel::for_sell(&sell_form),
        &["SYMBOL", "QTY", "ACCRUES TO"],
    );

    // Residency (a config flow with a real builder): Effective date · State.
    let res_form = tui::entry::ResidencyForm {
        effective_date: Date(20_000),
        state: "VA".to_string(),
    };
    assert_labels_render(
        &rt,
        FormModel::for_residency(&res_form),
        &["EFFECTIVE DATE", "STATE"],
    );
}

/// Open the form and assert each expected tracked-caps label renders through the one
/// form-layout path (the FLOW-009 field layout, not synthetic chrome).
fn assert_labels_render(rt: &FakeRuntime, form: FormModel, expected_labels: &[&str]) {
    let kind = form.kind;
    let mut model = Model::new();
    model.open_entry(EntryContext::form(form));
    let s = render_string(&model, rt, 100, 24);
    for label in expected_labels {
        assert!(
            s.contains(label),
            "{kind:?} renders the per-flow label {label}: {s}"
        );
    }
}

// @spec TUI-ENTRY-FLOW-009
#[test]
fn idle_entry_mode_with_no_open_flow_renders_a_calm_panel() {
    // Entry mode with no open context renders a calm panel (never a fabricated
    // form). (TUI-ENTRY-FLOW-009)
    let mut model = Model::new();
    model.mode = Mode::Entry;
    let rt = empty_runtime();
    let s = render_string(&model, &rt, 90, 16);
    assert!(s.contains("Entry"), "the idle Entry panel names the mode");
    assert!(s.contains("compose"), "the idle panel hints the write loop");
}

// ===========================================================================
// TUI-ENTRY-LOT-006 — lot-picker render as a Table with a footer band.
// ===========================================================================

fn open_sell_picker_model(sale_qty: MicroShares) -> Model {
    let snap = multi_lot_snapshot();
    let picker = LotPicker::build(
        &snap,
        &"AMZN".to_string(),
        "Robinhood",
        sale_qty,
        Date(20_000),
    );
    let form = FormModel::new(FlowKind::Sell, vec![Field::new("Qty", "0")]);
    let mut model = Model::new();
    model.open_entry(EntryContext::with_picker(form, picker));
    model
}

// @spec TUI-ENTRY-LOT-006
#[test]
fn lot_picker_renders_as_a_table_with_a_running_total_in_the_footer_band() {
    // The picker renders as a Table — one row per open lot with the column header —
    // and the running `allocated N / M` sits in a footer band beneath the body.
    // (TUI-ENTRY-LOT-006)
    let mut model = open_sell_picker_model(ms(150));
    // Allocate 100 of 150 so the running total reads a partial.
    if let Some(p) = model.entry_top_mut().unwrap().picker.as_mut() {
        p.set_take("old", ms(100));
    }
    let rt = empty_runtime();
    let s = render_string(&model, &rt, 100, 24);

    // The table header columns.
    assert!(s.contains("LOT"), "the lot-id column header");
    assert!(s.contains("TERM"), "the term column header");
    assert!(s.contains("TAKE"), "the take input-cell column header");
    // One data row per open lot (the take cell shows the allocation).
    assert!(s.contains("take ["), "a per-lot take input cell renders");
    // The running total in the footer band.
    assert!(
        s.contains("allocated"),
        "the running allocated N / M is in the footer band"
    );
    // The footer hint line offers FIFO + submit.
    assert!(s.contains("[F] FIFO"), "the picker footer offers FIFO-fill");
}

// @spec TUI-ENTRY-LOT-006
#[test]
fn lot_picker_footer_holds_the_est_preview_never_over_a_data_row() {
    // The [est] gain/tax preview sits in the footer band, beneath the table body —
    // never painted over a lot data row. (TUI-ENTRY-LOT-006/005)
    let mut model = open_sell_picker_model(ms(100));
    {
        let ctx = model.entry_top_mut().unwrap();
        ctx.picker.as_mut().unwrap().set_take("old", ms(100));
        ctx.preview = Some(GainTaxPreview {
            est_gain: Some(Cents(26_840_00)),
            est_tax: Some(Cents(8_120_00)),
            degraded: false,
        });
    }
    let rt = empty_runtime();
    let s = render_string(&model, &rt, 100, 24);
    assert!(s.contains("est. gain"), "the est. gain preview renders");
    assert!(s.contains("[est]"), "the preview is [est]-flagged");

    // The preview line is positioned strictly below the last lot data row (the
    // footer band, not over a data row). Locate the lot row and the preview row.
    let lines: Vec<&str> = s.lines().collect();
    let last_lot_row = lines
        .iter()
        .rposition(|l| l.contains("take ["))
        .expect("a lot data row");
    let preview_row = lines
        .iter()
        .position(|l| l.contains("est. gain"))
        .expect("the preview row");
    assert!(
        preview_row > last_lot_row,
        "the preview is in the footer band beneath the data rows"
    );
}

// @spec TUI-ENTRY-LOT-006
// @spec TUI-ENTRY-LOT-004
#[test]
fn lot_picker_empty_state_renders_in_the_footer_band() {
    // No open lots for the symbol on the sale's platform → the explicit empty state
    // renders in the footer band (warn role). (TUI-ENTRY-LOT-006/004)
    let snap = multi_lot_snapshot();
    let picker = LotPicker::build(&snap, &"AMZN".to_string(), "Fidelity", ms(10), Date(20_000));
    let form = FormModel::new(FlowKind::Sell, vec![Field::new("Qty", "10")]);
    let mut model = Model::new();
    model.open_entry(EntryContext::with_picker(form, picker));

    let rt = empty_runtime();
    let s = render_string(&model, &rt, 100, 24);
    assert!(
        s.contains("no open lots for AMZN on Fidelity"),
        "the explicit empty state renders in the footer band: {s}"
    );
}

// @spec TUI-ENTRY-LOT-006
#[test]
fn lot_picker_inline_error_renders_in_the_footer_band_not_over_a_data_row() {
    // The inline error sits in the footer band, never over a data row.
    // (TUI-ENTRY-LOT-006)
    let mut model = open_sell_picker_model(ms(150));
    model.entry_top_mut().unwrap().form.set_error(
        0,
        &InlineError::Field("allocation exceeds remaining".to_string()),
    );
    let rt = empty_runtime();
    let s = render_string(&model, &rt, 100, 24);
    assert!(
        s.contains("allocation exceeds remaining"),
        "the inline error renders in the footer band"
    );

    let lines: Vec<&str> = s.lines().collect();
    let last_lot_row = lines
        .iter()
        .rposition(|l| l.contains("take ["))
        .expect("a lot data row");
    let err_row = lines
        .iter()
        .position(|l| l.contains("allocation exceeds remaining"))
        .expect("the error row");
    assert!(
        err_row > last_lot_row,
        "the error is beneath the data rows, never over one"
    );
}

// ===========================================================================
// TUI-ENTRY-FLOW-008 — the input/keybinding layer (pure Model methods).
// ===========================================================================

// @spec TUI-ENTRY-FLOW-008
#[test]
fn tab_and_shift_tab_move_between_fields_wrapping() {
    // tab / shift-tab (and ↑/↓) move between fields; focus wraps. (TUI-ENTRY-FLOW-008)
    let mut model = open_buy_model();
    assert_eq!(
        model.entry_top().unwrap().form.focus,
        0,
        "focus starts on the first field"
    );

    model.entry_focus_next();
    assert_eq!(model.entry_top().unwrap().form.focus, 1);
    model.entry_focus_prev();
    assert_eq!(model.entry_top().unwrap().form.focus, 0);

    // shift-tab from the first field wraps to the last.
    model.entry_focus_prev();
    let n = model.entry_top().unwrap().form.fields.len();
    assert_eq!(
        model.entry_top().unwrap().form.focus,
        n - 1,
        "shift-tab wraps to the last field"
    );
    // tab from the last wraps to the first.
    model.entry_focus_next();
    assert_eq!(
        model.entry_top().unwrap().form.focus,
        0,
        "tab wraps to the first field"
    );
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn enter_advances_fields_then_submits_on_the_last_field() {
    // enter advances field-to-field; enter on the LAST field submits.
    // (TUI-ENTRY-FLOW-008)
    let mut model = open_buy_model();
    let n = model.entry_top().unwrap().form.fields.len();
    for i in 0..n - 1 {
        let action = model.entry_enter();
        assert_eq!(
            action,
            FormAction::Consumed,
            "enter on field {i} advances, not submits"
        );
    }
    // Now on the last field.
    assert!(model.entry_top().unwrap().form.on_last_field());
    let action = model.entry_enter();
    assert_eq!(
        action,
        FormAction::Submit,
        "enter on the last field submits"
    );
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn esc_cancels_and_pops_the_context_when_clean() {
    // esc in field-navigation mode with no dirty field cancels the flow and pops
    // the context; the entry stack empties and the mode falls back to Views.
    // (TUI-ENTRY-FLOW-008)
    let mut model = open_buy_model();
    assert_eq!(model.mode, Mode::Entry);
    let action = model.entry_escape();
    assert_eq!(action, FormAction::Cancel, "a clean esc cancels");
    assert!(model.entry_top().is_none(), "the context was popped");
    assert_eq!(
        model.mode,
        Mode::Views,
        "the empty entry stack falls back to Views"
    );
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn esc_with_a_dirty_field_requests_confirm_discard_without_popping() {
    // esc with any dirty field returns ConfirmDiscard WITHOUT popping — the shell
    // raises the confirm, then calls entry_discard on confirmation.
    // (TUI-ENTRY-FLOW-008)
    let mut model = open_buy_model();
    {
        let form = &mut model.entry_top_mut().unwrap().form;
        form.enter_field(); // text-input mode on the focused field
        form.type_char('A'); // edits → dirty
    }
    // Leave text-input mode first (esc from text-input returns to field-nav).
    let action = model.entry_escape();
    assert_eq!(
        action,
        FormAction::Consumed,
        "esc from text-input returns to field-nav, not cancel"
    );
    assert_eq!(
        model.entry_top().unwrap().form.input_mode,
        InputMode::FieldNav
    );

    // Now esc in field-nav with the dirty field requests a confirm-discard.
    assert!(
        model.entry_top().unwrap().form.is_dirty(),
        "the edited field is dirty"
    );
    let action = model.entry_escape();
    assert_eq!(
        action,
        FormAction::ConfirmDiscard,
        "a dirty esc requests confirm-discard"
    );
    assert!(
        model.entry_top().is_some(),
        "the context is NOT popped before confirmation"
    );

    // On confirmation the shell discards.
    model.entry_discard();
    assert!(
        model.entry_top().is_none(),
        "confirmation discards the context"
    );
    assert_eq!(model.mode, Mode::Views);
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn a_focused_field_enters_text_input_mode_suppressing_global_accelerators() {
    // A focused field enters text-input mode in which the shell's global single-key
    // accelerators are suppressed; esc returns to field-navigation mode.
    // (TUI-ENTRY-FLOW-008)
    let mut model = open_buy_model();
    assert!(
        !model.entry_in_text_input(),
        "starts in field-navigation mode (accelerators live)"
    );

    model.entry_enter_field();
    assert!(
        model.entry_in_text_input(),
        "the focused field is now in text-input mode"
    );

    // In text-input mode tab/shift-tab are the field's keystroke, not focus moves.
    let focus_before = model.entry_top().unwrap().form.focus;
    model.entry_focus_next();
    assert_eq!(
        model.entry_top().unwrap().form.focus,
        focus_before,
        "tab is suppressed as a focus move while in text-input mode"
    );

    // The footer suppresses the command accelerators in text-input mode.
    let hint = model.entry_top().unwrap().form.footer_hint();
    assert!(
        !hint.contains("[enter] submit"),
        "global accelerators are suppressed in text-input mode"
    );
    assert!(
        hint.contains("[esc] field"),
        "only the field's own escape applies in text-input mode"
    );

    // esc returns to field-navigation mode (NOT a flow cancel).
    let action = model.entry_escape();
    assert_eq!(
        action,
        FormAction::Consumed,
        "esc in text-input returns to field-nav, not cancel"
    );
    assert!(
        !model.entry_in_text_input(),
        "back in field-navigation mode"
    );
    assert!(model.entry_top().is_some(), "the flow is not cancelled");
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn the_active_context_is_the_top_stack_frame_and_esc_pops_it() {
    // The active keymap context is the top stack frame; opening a second context
    // makes it active, and esc pops back to the first. (TUI-ENTRY-FLOW-008)
    let mut model = open_buy_model(); // first context: Buy
    model.open_entry(EntryContext::form(FormModel::for_split(&SplitForm {
        symbol: "AMZN".to_string(),
        ratio_num: 2,
        ratio_den: 1,
        date: Date(20_000),
    })));
    assert_eq!(
        model.entry_top().unwrap().form.kind,
        FlowKind::Split,
        "the top frame is active"
    );

    let action = model.entry_escape();
    assert_eq!(action, FormAction::Cancel);
    assert_eq!(
        model.entry_top().unwrap().form.kind,
        FlowKind::Buy,
        "esc pops back to the parent context"
    );
    assert_eq!(
        model.mode,
        Mode::Entry,
        "a remaining context keeps Entry mode"
    );
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn typing_into_a_field_edits_its_value_and_marks_it_dirty() {
    // Typing into the focused field (text-input mode) edits its value + marks it
    // dirty; backspace removes a char. Outside text-input mode keystrokes are no-ops.
    // (TUI-ENTRY-FLOW-008)
    let mut model = open_buy_model();
    // Outside text-input mode, type_char is a no-op (the key is an accelerator).
    model.entry_top_mut().unwrap().form.type_char('X');
    assert!(
        !model.entry_top().unwrap().form.is_dirty(),
        "typing is suppressed outside text-input mode"
    );

    model.entry_enter_field();
    {
        let form = &mut model.entry_top_mut().unwrap().form;
        form.type_char('Z');
        form.type_char('M');
    }
    let f = &model.entry_top().unwrap().form.fields[0];
    assert!(
        f.value.ends_with("ZM"),
        "the typed chars land in the field value: {}",
        f.value
    );
    assert!(f.dirty, "an edited field is dirty");

    model.entry_top_mut().unwrap().form.backspace();
    assert!(model.entry_top().unwrap().form.fields[0]
        .value
        .ends_with('Z'));
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn form_builders_seed_fields_for_buy_vest_sell_in_tab_order() {
    // The per-flow builders seed the fields in tab order from the composed form;
    // rendering is a projection of these. (TUI-ENTRY-FLOW-008/009)
    let rt = empty_runtime();
    let platforms = config::PlatformList::new(vec!["Robinhood".to_string()]);
    let aliases = config::AliasMap::new(std::collections::BTreeMap::new());
    let residency = config::ResidencyTimeline::from_entries(vec![config::ResidencyEntry {
        effective_date: Date(0),
        state_code: "DC".to_string(),
    }])
    .unwrap();

    let buy = BuyForm::with_defaults(&rt, "L1".to_string(), "AMZN", &platforms, &aliases);
    let bf = FormModel::for_buy(&buy);
    assert_eq!(bf.fields[0].label, "Symbol", "Symbol is the first field");
    assert!(
        bf.fields.iter().any(|f| f.label == "Fees"),
        "Buy has a Fees field"
    );

    let vest = VestForm::with_defaults(&rt, "V1".to_string(), "AMZN", &platforms, &aliases);
    let vf = FormModel::for_vest(&vest);
    assert!(
        vf.fields.iter().any(|f| f.label == "FMV/share"),
        "Vest has FMV/share"
    );
    assert!(
        !vf.fields.iter().any(|f| f.label == "Fees"),
        "Vest has no Fees field"
    );

    let sell = SellForm::with_defaults(
        &rt,
        "S1".to_string(),
        "AMZN",
        &residency,
        &platforms,
        &aliases,
    );
    let sf = FormModel::for_sell(&sell);
    assert!(
        sf.fields.iter().any(|f| f.label == "Accrues to"),
        "Sell carries accrues-to-state"
    );
    assert_eq!(sf.kind, FlowKind::Sell);
}
