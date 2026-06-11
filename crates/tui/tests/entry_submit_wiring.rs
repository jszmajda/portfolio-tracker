//! The live composer **submit driver**, the **field-string → typed-candidate
//! re-parse**, the **lot-picker keymap**, and the **byte-identical-retry freeze** —
//! the Model-level wiring the binary's event loop drives, exercised against the FAKE
//! runtime (no terminal, no Sheets). These pin the half the render/route tests do
//! not reach: that an OPEN composer's `enter` actually re-parses, composes, submits
//! through the port, and folds the returned `Phase` back — and that a `[r]etry`
//! re-submits the frozen byte-identical event without re-deriving any field.
//! (TUI-ENTRY-FLOW-002/003/004/005/008/010, TUI-ENTRY-ACT-006, TUI-ENTRY-LOT-001/002/003,
//! CONFIG-RESIDENCY-005)

mod common;

use std::collections::BTreeMap;

use ledger_core::LedgerEventKind;
use pt_core::{Cents, Date, MicroShares};
use tui::entry::{Composer, Phase, SellForm};
use tui::testkit::{buy, flat_federal_ctx, replay, FakeRuntime, SubmitBehavior, ViewBuilder};
use tui::{Mode, Model};

fn ms(n: i64) -> MicroShares {
    MicroShares(n * pt_core::SHARE_SCALE)
}

fn empty_runtime() -> FakeRuntime {
    let snap = ledger_core::Snapshot::default();
    FakeRuntime::new(ViewBuilder::new(snap).build(), flat_federal_ctx(common::YEAR, 220_000))
}

fn platforms() -> config::PlatformList {
    config::PlatformList::new(vec!["Robinhood".to_string()])
}

fn aliases_with(mappings: &[(&str, &str)]) -> config::AliasMap {
    let mut m = BTreeMap::new();
    for (k, v) in mappings {
        m.insert(k.to_string(), v.to_string());
    }
    config::AliasMap::new(m)
}

/// Replace the focused field's value with `s` (the owner clears the seeded default,
/// then types): enter text-input, backspace the seeded content away, then type.
fn type_into_focused(model: &mut Model, s: &str) {
    model.entry_enter_field();
    // Clear the seeded value first (a real owner overtypes the default).
    let seeded_len = model
        .entry_top()
        .and_then(|c| c.form.fields.get(c.form.focus))
        .map(|f| f.value.chars().count())
        .unwrap_or(0);
    for _ in 0..seeded_len {
        model.entry_backspace();
    }
    for c in s.chars() {
        model.entry_type_char(c);
    }
}

// ===========================================================================
// TUI-ENTRY-FLOW-003/008 — the live submit driver: a Buy composed from the edited
// fields confirms durable and clears the context.
// ===========================================================================

// @spec TUI-ENTRY-FLOW-003, TUI-ENTRY-FLOW-008, TUI-ENTRY-ACT-001
#[test]
fn open_buy_edit_fields_and_submit_confirms_durable_and_clears_the_context() {
    let mut rt = empty_runtime();
    let mut model = Model::new();
    model.toggle_mode(); // Entry mode
    model.open_buy(&rt, "L1".to_string(), "AMZN", &platforms(), &aliases_with(&[]));
    assert_eq!(model.mode, Mode::Entry);
    assert!(model.entry_top().is_some(), "the composer is open");

    // Edit the seeded fields: qty 10, unit price $100. (The symbol/platform/date were
    // seeded by with_defaults; the qty/price seeded to 0 must be edited.)
    // Field order (for_buy): Symbol, Qty, Unit price, Date, Fees, Platform, Tracking.
    model.entry_focus_next(); // → Qty
    type_into_focused(&mut model, "10");
    // Leave text-input, advance to Unit price.
    model.entry_escape(); // back to field-nav
    model.entry_focus_next(); // → Unit price
    type_into_focused(&mut model, "100");
    model.entry_escape();

    // A new-position confirm: AMZN is unknown on the empty snapshot, so the first
    // submit holds on the new-symbol guard. (TUI-ENTRY-ACT-006)
    let aliases = aliases_with(&[]);
    model.entry_submit(&mut rt, &aliases);
    assert!(
        model.entry_phase_confirm_new_symbol(),
        "an unknown symbol holds on the new-symbol guard, nothing written"
    );
    assert!(rt.ledger_log.is_empty(), "the new-symbol hold writes nothing");

    // Confirm the new position + re-submit: now it confirms durable and clears.
    model.entry_confirm_new_symbol(&mut rt, &aliases);
    assert!(model.entry_top().is_none(), "a confirmed-durable submit clears the composer");
    assert_eq!(model.mode, Mode::Views, "the empty entry stack falls back to Views");
    assert_eq!(rt.ledger_log.len(), 1, "the Buy landed durably");
    // The composed Buy carries the edited values.
    match &rt.ledger_log[0].kind {
        LedgerEventKind::Buy { symbol, qty, unit_price_cents, .. } => {
            assert_eq!(symbol, "AMZN");
            assert_eq!(*qty, ms(10), "the edited qty composed");
            assert_eq!(*unit_price_cents, Cents(100_00), "the edited $100 unit price composed");
        }
        other => panic!("expected a Buy, got {other:?}"),
    }
}

// ===========================================================================
// TUI-ENTRY-FLOW-002 — a submit-time kernel disagreement re-renders inline beside
// the field, nothing written, and the context is preserved (not cleared).
// ===========================================================================

// @spec TUI-ENTRY-FLOW-002, TUI-ENTRY-FLOW-008
#[test]
fn submit_disagreement_rerenders_inline_and_preserves_the_context() {
    let mut rt = empty_runtime()
        .with_behavior(SubmitBehavior::SubmitRejectLedger(ledger_core::LedgerError::InsufficientShares));
    let mut model = Model::new();
    model.toggle_mode();
    // A known symbol (so the new-symbol guard does not intercept): seed a position.
    let snap = replay(&[buy(1, 18_000, "L0", "AMZN", 5, 10_000, "Robinhood")], &[("AMZN", 26_126)]);
    rt.set_view(ViewBuilder::new(snap).build());
    rt = rt.with_ledger_log(vec![buy(1, 18_000, "L0", "AMZN", 5, 10_000, "Robinhood")]);

    model.open_buy(&rt, "L1".to_string(), "AMZN", &platforms(), &aliases_with(&[]));
    model.entry_focus_next();
    type_into_focused(&mut model, "10");
    model.entry_escape();
    model.entry_focus_next();
    type_into_focused(&mut model, "100");
    model.entry_escape();

    model.entry_submit(&mut rt, &aliases_with(&[]));
    let ctx = model.entry_top().expect("the context is preserved on a submit-time rejection");
    match &ctx.form.phase {
        Phase::Rejected(e) => assert!(e.text().contains("InsufficientShares"), "the kernel error rides inline"),
        other => panic!("expected a Rejected phase, got {other:?}"),
    }
    assert!(ctx.form.error.is_some(), "the inline slot carries the kernel error beside a field");
    assert!(rt.ledger_log.iter().all(|e| e.id != "evt-2"), "nothing new was written");
}

// ===========================================================================
// TUI-ENTRY-FLOW-008 / ACT-001 — the field-string → typed-candidate re-parse: a
// malformed or empty numeric field surfaces a graceful inline error, never a panic.
// ===========================================================================

// @spec TUI-ENTRY-FLOW-008, TUI-ENTRY-ACT-001
#[test]
fn a_malformed_or_empty_numeric_field_surfaces_an_inline_error_not_a_panic() {
    let mut rt = empty_runtime();
    let mut model = Model::new();
    model.toggle_mode();
    model.open_buy(&rt, "L1".to_string(), "AMZN", &platforms(), &aliases_with(&[]));

    // Type garbage into the Qty field.
    model.entry_focus_next(); // → Qty
    type_into_focused(&mut model, "abc");
    model.entry_escape();

    model.entry_submit(&mut rt, &aliases_with(&[]));
    let ctx = model.entry_top().expect("a parse error preserves the context");
    match &ctx.form.phase {
        Phase::Rejected(e) => assert!(
            e.text().contains("share count"),
            "a non-numeric qty surfaces a composer-local parse error: {}",
            e.text()
        ),
        other => panic!("expected a parse Rejected, got {other:?}"),
    }
    assert!(rt.ledger_log.is_empty(), "a parse error writes nothing");

    // An oversized qty also surfaces a graceful error (no overflow panic).
    {
        let f = &mut model.entry_top_mut().unwrap().form;
        f.fields[1].value = "9999999999999999999999".to_string();
    }
    model.entry_submit(&mut rt, &aliases_with(&[]));
    assert!(
        matches!(model.entry_top().unwrap().form.phase, Phase::Rejected(_)),
        "an oversized qty surfaces a graceful inline error, not a panic"
    );
    assert!(rt.ledger_log.is_empty());
}

// ===========================================================================
// CONFIG-RESIDENCY-005 — a manual Sell with no resolved residency default blocks
// submit with the inline error beside the accrues-to-state field.
// ===========================================================================

// @spec CONFIG-RESIDENCY-005, TUI-ENTRY-FLOW-008
#[test]
fn a_sell_with_no_residency_default_blocks_submit_with_the_inline_error() {
    let snap = replay(&[buy(1, 18_000, "L0", "AMZN", 10, 10_000, "Robinhood")], &[("AMZN", 26_126)]);
    let mut rt = FakeRuntime::new(ViewBuilder::new(snap.clone()).build(), flat_federal_ctx(common::YEAR, 220_000))
        .with_ledger_log(vec![buy(1, 18_000, "L0", "AMZN", 10, 10_000, "Robinhood")]);
    let mut model = Model::new();
    model.toggle_mode();
    // An EMPTY residency timeline → residency_on returns None → accrues_to_state None.
    let empty_residency = config::ResidencyTimeline::new();
    model.open_sell(&rt, "S1".to_string(), "AMZN", &empty_residency, &platforms(), &aliases_with(&[]));

    // Submit (the picker need not be complete to hit the residency guard, which runs
    // first in submit_sell).
    model.entry_submit(&mut rt, &aliases_with(&[]));
    let ctx = model.entry_top().expect("the residency block preserves the context");
    match &ctx.form.phase {
        Phase::Rejected(e) => assert!(
            e.text().contains("no residency on the sale date"),
            "the residency block names the missing state: {}",
            e.text()
        ),
        other => panic!("expected a residency-block Rejected, got {other:?}"),
    }
    assert!(rt.ledger_log.iter().all(|e| e.id != "evt-2"), "the residency block writes nothing");
}

// ===========================================================================
// TUI-ENTRY-LOT-001/002/003 — the lot-picker keymap: focus movement, FIFO-fill, a
// per-row take, and the qty-change allocation reset, all driven through the Model.
// ===========================================================================

fn open_sell_two_lots(rt: &FakeRuntime, model: &mut Model, sale_qty: MicroShares) {
    // Two AMZN lots on Robinhood (older 100, newer 100).
    let log = vec![
        buy(1, 15_000, "old", "AMZN", 100, 800, "Robinhood"),
        buy(2, 19_000, "new", "AMZN", 100, 17_200, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let form = SellForm {
        sale_id: "S1".to_string(),
        symbol: "AMZN".to_string(),
        qty: sale_qty,
        unit_price: Cents(26_000),
        fees: Cents(0),
        date: Date(20_000),
        platform: "Robinhood".to_string(),
        tracking_code: None,
        accrues_to_state: Some("DC".to_string()),
        picker: tui::entry::LotPicker::build(&snap, &"AMZN".to_string(), "Robinhood", sale_qty, Date(20_000)),
    };
    let _ = rt;
    model.toggle_mode();
    model.open_composer(Composer::Sell(form));
}

// @spec TUI-ENTRY-LOT-001, TUI-ENTRY-LOT-006
#[test]
fn picker_focus_moves_and_fifo_fill_allocates_oldest_first_through_the_model() {
    let rt = empty_runtime();
    let mut model = Model::new();
    open_sell_two_lots(&rt, &mut model, ms(150));
    assert!(model.entry_has_picker(), "a Sell context has an operable picker");

    // Focus starts at row 0; move down then back up (capped).
    assert_eq!(model.entry_top().unwrap().picker_focus, 0);
    model.picker_focus_next();
    assert_eq!(model.entry_top().unwrap().picker_focus, 1);
    model.picker_focus_next();
    assert_eq!(model.entry_top().unwrap().picker_focus, 1, "capped at the last row");
    model.picker_focus_prev();
    assert_eq!(model.entry_top().unwrap().picker_focus, 0);

    // FIFO-fill allocates oldest-first up to the sale qty (150 = 100 old + 50 new).
    model.picker_fill_fifo();
    let picker = model.entry_top().unwrap().picker_ref().unwrap();
    let old = picker.lots.iter().find(|l| l.lot_id == "old").unwrap();
    let new = picker.lots.iter().find(|l| l.lot_id == "new").unwrap();
    assert_eq!(old.take, ms(100), "FIFO fills the oldest lot first");
    assert_eq!(new.take, ms(50), "then the next-oldest for the remainder");
    assert_eq!(picker.allocated(), ms(150));
    assert!(picker.is_complete(), "the allocation sums exactly to the sale qty");
}

// @spec TUI-ENTRY-LOT-002
#[test]
fn picker_set_take_on_focus_caps_at_remaining() {
    let rt = empty_runtime();
    let mut model = Model::new();
    open_sell_two_lots(&rt, &mut model, ms(150));

    // Set a take of 999 on the focused row (old, rem 100): capped at 100.
    model.picker_set_take_on_focus(ms(999));
    let picker = model.entry_top().unwrap().picker_ref().unwrap();
    let old = picker.lots.iter().find(|l| l.lot_id == "old").unwrap();
    assert_eq!(old.take, ms(100), "the take caps at the lot's remaining");
}

// @spec TUI-ENTRY-LOT-003, TUI-ENTRY-FLOW-008
#[test]
fn editing_the_sell_qty_resets_the_allocation() {
    let rt = empty_runtime();
    let mut model = Model::new();
    open_sell_two_lots(&rt, &mut model, ms(150));
    // FIFO-fill, then change the qty field and re-parse: the allocation resets.
    model.picker_fill_fifo();
    assert!(model.entry_top().unwrap().picker_ref().unwrap().allocated().0 > 0);

    // Edit the Qty field (index 1) to 200 and re-parse via a submit attempt; the
    // SellForm::apply_fields resets the allocation when the qty changes. (LOT-003)
    {
        let f = &mut model.entry_top_mut().unwrap().form;
        f.fields[1].value = "200".to_string();
    }
    // Re-parse without committing by composing through the typed composer path.
    let mut rt2 = rt;
    model.entry_submit(&mut rt2, &aliases_with(&[]));
    let picker = model.entry_top().unwrap().picker_ref().unwrap();
    assert_eq!(picker.sale_qty, ms(200), "the sale qty updated from the field");
    assert_eq!(picker.allocated(), MicroShares(0), "changing the qty reset the allocation");
}

// ===========================================================================
// TUI-ENTRY-FLOW-010 — the byte-identical retry freeze: a live retry re-submits the
// frozen composed event, re-deriving NO field (no fresh today, no re-resolved
// alias) between attempts.
// ===========================================================================

// @spec TUI-ENTRY-FLOW-010, TUI-ENTRY-FLOW-004
#[test]
fn a_live_retry_resubmits_the_frozen_event_without_rederiving_today_or_alias() {
    // First submit returns control (unreachable). The composer FREEZES the composed
    // event. Then `today` advances and the alias map changes; a retry must re-submit
    // the byte-identical frozen event (same date, same resolved symbol), not re-derive.
    let mut rt = empty_runtime().with_behavior(SubmitBehavior::Unreachable);
    let mut model = Model::new();
    model.toggle_mode();
    // A known symbol so the new-symbol guard does not intercept: seed AMZN.
    let snap = replay(&[buy(1, 18_000, "L0", "AMZN", 10, 10_000, "Robinhood")], &[("AMZN", 26_126)]);
    rt.set_view(ViewBuilder::new(snap).build());
    let alias_v1 = aliases_with(&[("AMZN", "AMZN")]);
    model.open_buy(&rt, "L1".to_string(), "AMZN", &platforms(), &alias_v1);
    // Edit qty + price so the candidate is well-formed.
    model.entry_focus_next();
    type_into_focused(&mut model, "10");
    model.entry_escape();
    model.entry_focus_next();
    type_into_focused(&mut model, "100");
    model.entry_escape();

    // First submit: unreachable → Retry, and the composed event is frozen.
    model.entry_submit(&mut rt, &alias_v1);
    assert!(
        matches!(model.entry_top().unwrap().form.phase, Phase::Retry(_)),
        "an unreachable submit returns control with [r]etry"
    );
    let frozen = model
        .entry_top()
        .unwrap()
        .frozen
        .clone()
        .expect("the composed event is frozen at first submit");

    // The world drifts: `today` would now be different, and an alias remaps AMZN.
    let alias_v2 = aliases_with(&[("AMZN", "AMZN_RENAMED")]);
    // The condition clears so a retry can land.
    rt.behavior = SubmitBehavior::Confirm;

    // Retry (the `[r]` path re-submits the FROZEN event — the binary's EntryEnter on a
    // Retry phase calls entry_submit, which reuses the frozen candidate).
    model.entry_submit(&mut rt, &alias_v2);
    assert!(model.entry_top().is_none(), "the retry confirmed durable and cleared");
    assert_eq!(rt.ledger_log.len(), 1, "the retry landed exactly once");

    // The landed event is byte-identical to the frozen one (same date, same symbol) —
    // NOT re-derived from the drifted alias map.
    let tui::entry::Candidate::Ledger(frozen_ev) = frozen else { panic!("a ledger candidate") };
    match (&frozen_ev.kind, &rt.ledger_log[0].kind) {
        (
            LedgerEventKind::Buy { symbol: fs, qty: fq, .. },
            LedgerEventKind::Buy { symbol: ls, qty: lq, .. },
        ) => {
            assert_eq!(fs, "AMZN", "the frozen symbol is the original resolution");
            assert_eq!(ls, fs, "the retry submitted the frozen symbol, not the remapped alias");
            assert_eq!(lq, fq, "the retry submitted the frozen qty");
        }
        other => panic!("expected Buy events, got {other:?}"),
    }
    assert_eq!(frozen_ev.date, rt.ledger_log[0].date, "the retry kept the frozen date (no fresh today)");
}



// ===========================================================================
// TUI-ENTRY-FLOW-010 — an edit after a returned-control submit is a NEW entry: the
// freeze is dropped, so the next submit re-derives from the (edited) fields.
// ===========================================================================

// @spec TUI-ENTRY-FLOW-010
#[test]
fn an_edit_after_a_returned_control_submit_drops_the_freeze() {
    let mut rt = empty_runtime().with_behavior(SubmitBehavior::Unreachable);
    let mut model = Model::new();
    model.toggle_mode();
    let snap = replay(&[buy(1, 18_000, "L0", "AMZN", 10, 10_000, "Robinhood")], &[("AMZN", 26_126)]);
    rt.set_view(ViewBuilder::new(snap).build());
    model.open_buy(&rt, "L1".to_string(), "AMZN", &platforms(), &aliases_with(&[]));
    model.entry_focus_next();
    type_into_focused(&mut model, "10");
    model.entry_escape();
    model.entry_focus_next();
    type_into_focused(&mut model, "100");
    model.entry_escape();

    model.entry_submit(&mut rt, &aliases_with(&[]));
    assert!(model.entry_top().unwrap().frozen.is_some(), "the unreachable submit froze the event");

    // An edit drops the freeze (a new entry, new content, new id). (FLOW-010)
    model.entry_focus_next(); // → Date (or wherever); enter text-input + type.
    type_into_focused(&mut model, "1");
    assert!(
        model.entry_top().unwrap().frozen.is_none(),
        "editing a field after a returned-control submit drops the frozen candidate"
    );
}
