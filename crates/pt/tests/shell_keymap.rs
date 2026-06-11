//! The TUI shell's key-routing decision (TUI-ENTRY-FLOW-008). The binary's crossterm
//! loop translates a key into a `ShellKey` and routes it via `pt::shell::route`; these
//! pin the routing decision — the active context owns the keymap, text-input mode
//! suppresses the global accelerators, and an unbound Entry key (and every Views key)
//! falls through to the global bindings — purely, with no TTY.

use pt::shell::{route, KeyRoute, LaunchFlow, ShellKey};
use tui::entry::{BuyForm, FlowKind, Phase};
use tui::form::{Field, FormModel};
use tui::port::RuntimePort;
use tui::testkit::{buy, flat_federal_ctx, replay, FakeRuntime, ViewBuilder};
use tui::{EntryContext, Mode, Model};

use pt_core::{Cents, Date, MicroShares};

fn a_buy() -> BuyForm {
    BuyForm {
        lot_id: "L1".to_string(),
        symbol: "AMZN".to_string(),
        qty: MicroShares(0),
        unit_price: Cents(0),
        fees: Cents(0),
        date: Date(20_000),
        platform: "Robinhood".to_string(),
        tracking_code: None,
        confirmed_new_symbol: false,
    }
}

/// A Model in Views mode, at the landing frame (no open composer).
fn views_model() -> Model {
    Model::new()
}

/// A Model in Entry mode with an open Buy composer (field-navigation mode).
fn entry_model() -> Model {
    let mut m = Model::new();
    m.open_entry(EntryContext::form(FormModel::for_buy(&a_buy())));
    m
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn views_mode_routes_keys_to_the_global_bindings() {
    let m = views_model();
    // At the landing frame, `q`/`esc` quit; `tab` toggles mode; `r` refreshes.
    assert_eq!(route(&m, ShellKey::Char('q')), KeyRoute::Quit);
    assert_eq!(
        route(&m, ShellKey::Esc),
        KeyRoute::Quit,
        "esc at the landing frame quits"
    );
    assert_eq!(route(&m, ShellKey::Tab), KeyRoute::ToggleMode);
    assert_eq!(route(&m, ShellKey::Char('r')), KeyRoute::Refresh);
    // An unbound key is a no-op in Views.
    assert_eq!(route(&m, ShellKey::Char('z')), KeyRoute::Noop);
}

// @spec TUI-VIEW-NAV-014
#[test]
fn views_mode_binds_one_through_five_to_the_screens_everywhere_in_views() {
    use tui::views::Screen;
    let mut m = views_model();
    // `1`–`5` reach each screen from the landing frame...
    assert_eq!(
        route(&m, ShellKey::Char('1')),
        KeyRoute::SwitchScreen(Screen::Positions)
    );
    assert_eq!(
        route(&m, ShellKey::Char('2')),
        KeyRoute::SwitchScreen(Screen::OpenLots)
    );
    assert_eq!(
        route(&m, ShellKey::Char('3')),
        KeyRoute::SwitchScreen(Screen::History)
    );
    assert_eq!(
        route(&m, ShellKey::Char('4')),
        KeyRoute::SwitchScreen(Screen::TaxReserves)
    );
    assert_eq!(
        route(&m, ShellKey::Char('5')),
        KeyRoute::SwitchScreen(Screen::Realized)
    );
    // ...and from a drilled frame too (the switch drops the drill scope).
    assert!(m.drill(&tui::views::RowIdentity::Symbol("AMZN".to_string())));
    assert_eq!(
        route(&m, ShellKey::Char('5')),
        KeyRoute::SwitchScreen(Screen::Realized)
    );
    // `6`+ stays unbound.
    assert_eq!(route(&m, ShellKey::Char('6')), KeyRoute::Noop);
}

// @spec TUI-VIEW-NAV-014, TUI-ENTRY-FLOW-008
#[test]
fn digits_do_not_switch_screens_inside_an_entry_composer_or_text_input() {
    use tui::views::Screen;
    // Inside an open composer, a digit starts typing into the field — it must
    // never be swallowed as a screen switch.
    let m = entry_model();
    assert_eq!(
        route(&m, ShellKey::Char('3')),
        KeyRoute::EntryStartTyping('3')
    );
    // In text-input mode a digit types into the field.
    let mut m = entry_model();
    m.entry_enter_field();
    assert!(m.entry_in_text_input());
    assert_eq!(route(&m, ShellKey::Char('3')), KeyRoute::EntryType('3'));
    // The idle Entry panel binds no screen digits (tab back to Views first).
    let mut m = views_model();
    m.toggle_mode();
    assert_eq!(route(&m, ShellKey::Char('3')), KeyRoute::Noop);
    let _ = Screen::History; // the screens are reachable from Views (above)
}

// @spec TUI-ENTRY-FLOW-008, TUI-VIEW-NAV-003
#[test]
fn esc_ascends_a_drilled_frame_but_quits_only_at_the_landing_frame() {
    let mut m = views_model();
    // Drill into Open Lots (a real drill target from Positions).
    let drilled = m.drill(&tui::views::RowIdentity::Symbol("AMZN".to_string()));
    if drilled {
        assert!(m.stack.len() > 1, "the drill pushed a frame");
        assert_eq!(
            route(&m, ShellKey::Esc),
            KeyRoute::Ascend,
            "esc on a drilled frame ascends"
        );
    }
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn an_open_entry_composer_owns_the_field_navigation_keymap() {
    let m = entry_model();
    assert_eq!(m.mode, Mode::Entry);
    // tab/↓ and shift-tab/↑ move focus; the active context owns these keys.
    assert_eq!(route(&m, ShellKey::Tab), KeyRoute::EntryFocusNext);
    assert_eq!(route(&m, ShellKey::Down), KeyRoute::EntryFocusNext);
    assert_eq!(route(&m, ShellKey::BackTab), KeyRoute::EntryFocusPrev);
    assert_eq!(route(&m, ShellKey::Up), KeyRoute::EntryFocusPrev);
    // enter commits/advances; esc cancels the flow.
    assert_eq!(route(&m, ShellKey::Enter), KeyRoute::EntryEnter);
    assert_eq!(route(&m, ShellKey::Esc), KeyRoute::EntryEscape);
    // A printable key starts editing the focused field (NOT a global accelerator) —
    // so `r`/`q` in Entry field-nav begin typing, never refresh/quit.
    assert_eq!(
        route(&m, ShellKey::Char('A')),
        KeyRoute::EntryStartTyping('A')
    );
    assert_eq!(
        route(&m, ShellKey::Char('r')),
        KeyRoute::EntryStartTyping('r')
    );
    assert_eq!(
        route(&m, ShellKey::Char('q')),
        KeyRoute::EntryStartTyping('q')
    );
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn text_input_mode_suppresses_global_accelerators_and_routes_to_the_field() {
    let mut m = entry_model();
    m.entry_enter_field(); // the focused field is now in text-input mode
    assert!(m.entry_in_text_input());

    // Printables type into the field; tab does NOT toggle mode (suppressed); backspace
    // and enter/esc route to the field. (TUI-ENTRY-FLOW-008)
    assert_eq!(route(&m, ShellKey::Char('1')), KeyRoute::EntryType('1'));
    assert_eq!(
        route(&m, ShellKey::Char('q')),
        KeyRoute::EntryType('q'),
        "a typed 'q' is text, not quit"
    );
    assert_eq!(
        route(&m, ShellKey::Char('r')),
        KeyRoute::EntryType('r'),
        "a typed 'r' is text, not refresh"
    );
    assert_eq!(route(&m, ShellKey::Backspace), KeyRoute::EntryBackspace);
    assert_eq!(route(&m, ShellKey::Enter), KeyRoute::EntryEnter);
    assert_eq!(
        route(&m, ShellKey::Esc),
        KeyRoute::EntryEscape,
        "esc leaves the field"
    );
    // Tab is suppressed in text-input mode (a no-op, not a mode toggle).
    assert_eq!(
        route(&m, ShellKey::Tab),
        KeyRoute::Noop,
        "tab is suppressed while typing"
    );
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn enter_advances_then_submits_on_the_last_field_via_the_routed_model_method() {
    // The route is EntryEnter on every field; the Model's entry_enter decides advance
    // vs submit. Drive the Model along the routed enters to confirm the binding reaches
    // FormAction::Submit on the last field. (TUI-ENTRY-FLOW-008)
    let mut m = entry_model();
    let n = m.entry_top().unwrap().form.fields.len();
    let mut last_action = tui::form::FormAction::Consumed;
    for _ in 0..n {
        assert_eq!(route(&m, ShellKey::Enter), KeyRoute::EntryEnter);
        last_action = m.entry_enter();
    }
    assert_eq!(
        last_action,
        tui::form::FormAction::Submit,
        "enter on the last field submits"
    );
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn a_views_key_in_entry_mode_with_no_open_composer_falls_through_to_global() {
    // Entry mode but the composer stack is empty (a flow was just popped): keys fall
    // through to the global bindings, so `tab` still toggles back to Views. The router
    // only hands keys to the Entry keymap while a context is OPEN. (TUI-ENTRY-FLOW-008)
    let mut m = Model::new();
    m.toggle_mode(); // Entry mode, but `entry` is empty (no open context)
    assert_eq!(m.mode, Mode::Entry);
    assert!(m.entry_top().is_none());
    assert_eq!(
        route(&m, ShellKey::Tab),
        KeyRoute::ToggleMode,
        "no open composer ⇒ global keymap"
    );
    assert_eq!(route(&m, ShellKey::Char('r')), KeyRoute::Refresh);
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn an_unbound_key_is_a_noop_in_an_entry_composer() {
    // A function-style key the keymap does not bind is a no-op in the active composer
    // (never silently mis-routed). (TUI-ENTRY-FLOW-008)
    let m = entry_model();
    assert_eq!(route(&m, ShellKey::Other), KeyRoute::Noop);
    // Sanity: a single-field form so the routing does not depend on field count.
    let mut single = Model::new();
    single.open_entry(EntryContext::form(FormModel::new(
        FlowKind::Split,
        vec![Field::new("Field", "x")],
    )));
    assert_eq!(route(&single, ShellKey::Other), KeyRoute::Noop);
}

// @spec TUI-VIEW-NAV-011
#[test]
fn question_mark_toggles_the_help_overlay_outside_text_input_mode() {
    // `?` in Views opens the modal help overlay; while it is up the overlay owns
    // the keymap (everything but esc/? is swallowed); esc or ? dismisses it.
    let mut m = views_model();
    assert_eq!(route(&m, ShellKey::Char('?')), KeyRoute::ToggleHelp);
    m.toggle_help();
    assert!(m.help_open);
    assert_eq!(
        route(&m, ShellKey::Char('r')),
        KeyRoute::Noop,
        "the modal swallows keys"
    );
    assert_eq!(
        route(&m, ShellKey::Char('q')),
        KeyRoute::Noop,
        "q never quits through the modal"
    );
    assert_eq!(
        route(&m, ShellKey::Esc),
        KeyRoute::ToggleHelp,
        "esc dismisses"
    );
    assert_eq!(
        route(&m, ShellKey::Char('?')),
        KeyRoute::ToggleHelp,
        "? dismisses"
    );

    // The idle Entry panel binds ? too.
    let mut idle = Model::new();
    idle.toggle_mode();
    assert_eq!(route(&idle, ShellKey::Char('?')), KeyRoute::ToggleHelp);

    // A composer in field-navigation mode binds ? ahead of start-typing.
    let composer = entry_model();
    assert_eq!(route(&composer, ShellKey::Char('?')), KeyRoute::ToggleHelp);
}

// @spec TUI-VIEW-NAV-011
#[test]
fn question_mark_is_text_in_text_input_mode_never_the_overlay() {
    // Text-input mode suppresses the help bind — a typed `?` is text. (the entry
    // keymap's suppression rule)
    let mut m = entry_model();
    m.entry_enter_field();
    assert!(m.entry_in_text_input());
    assert_eq!(route(&m, ShellKey::Char('?')), KeyRoute::EntryType('?'));
}

// @spec TUI-VIEW-NAV-010
#[test]
fn every_status_line_hint_routes_to_a_live_binding() {
    // The hints advertise only keys the shell actually binds: each hinted key
    // routes to a non-Noop route in the context that hinted it.
    let to_key = |k: &str| match k {
        "tab" => Some(ShellKey::Tab),
        "enter" => Some(ShellKey::Enter),
        "esc" => Some(ShellKey::Esc),
        k if k.chars().count() == 1 => k.chars().next().map(ShellKey::Char),
        _ => None, // composite hints like ↑/↓ and 0-9 are exercised by the picker tests
    };
    let mut contexts: Vec<(&str, Model)> = Vec::new();
    contexts.push(("views landing", views_model()));
    let mut drilled = views_model();
    assert!(drilled.drill(&tui::views::RowIdentity::Symbol("AMZN".to_string())));
    contexts.push(("views drilled", drilled));
    let mut idle = Model::new();
    idle.toggle_mode();
    contexts.push(("idle entry panel", idle));
    contexts.push(("open composer", entry_model()));
    for (name, model) in contexts {
        for (key, word) in tui::key_hints(&model) {
            if let Some(sk) = to_key(key) {
                assert_ne!(
                    route(&model, sk),
                    KeyRoute::Noop,
                    "{name}: the hinted [{key}] {word} must be a live binding"
                );
            }
        }
    }
}

// ===========================================================================
// The shell-driven LIVE wiring (TUI-ENTRY-FLOW-008/002/003/010, ACT-006,
// LOT-001/002): a key sequence routed through `pt::shell::route` and EXECUTED
// against the Model + a FAKE runtime — the half the pure routing tests above do
// not reach. `dispatch` mirrors the binary's `dispatch_key` for the routes these
// exercise, so a test drives a key run from the landing screen to an open composer,
// through the picker, to a confirmed submit.
// ===========================================================================

fn ms(n: i64) -> MicroShares {
    MicroShares(n * pt_core::SHARE_SCALE)
}

fn platforms() -> config::PlatformList {
    config::PlatformList::new(vec!["Robinhood".to_string()])
}

fn aliases() -> config::AliasMap {
    config::AliasMap::default()
}

fn residency_dc() -> config::ResidencyTimeline {
    config::ResidencyTimeline::from_entries(vec![config::ResidencyEntry {
        effective_date: Date(0),
        state_code: "DC".to_string(),
    }])
    .unwrap()
}

/// Execute one routed key against the Model + the fake port — the subset of the
/// binary's `dispatch_key` these tests drive (launch / nav / text-input / picker /
/// submit). Flow-launch seeds the composer through the port + `config` exactly as the
/// binary does. (TUI-ENTRY-FLOW-008)
fn dispatch(model: &mut Model, port: &mut FakeRuntime, key: ShellKey) {
    match route(model, key) {
        KeyRoute::ToggleMode => model.toggle_mode(),
        KeyRoute::Refresh => {
            let _ = model.refresh(port);
        }
        KeyRoute::Ascend => {
            model.ascend();
        }
        KeyRoute::SwitchScreen(screen) => model.switch_screen(screen),
        KeyRoute::OpenFlow(flow) => match flow {
            LaunchFlow::Buy => model.open_buy(port, "L1".to_string(), "", &platforms(), &aliases()),
            LaunchFlow::Vest => {
                model.open_vest(port, "V1".to_string(), "", &platforms(), &aliases())
            }
            LaunchFlow::Sell => model.open_sell(
                port,
                "S1".to_string(),
                "",
                &residency_dc(),
                &platforms(),
                &aliases(),
            ),
            LaunchFlow::Split => model.open_split("", port.today()),
        },
        KeyRoute::EntryFocusNext => {
            model.entry_focus_next();
        }
        KeyRoute::EntryFocusPrev => {
            model.entry_focus_prev();
        }
        KeyRoute::EntryStartTyping(c) => {
            model.entry_enter_field();
            model.entry_type_char(c);
        }
        KeyRoute::EntryType(c) => model.entry_type_char(c),
        KeyRoute::EntryBackspace => model.entry_backspace(),
        KeyRoute::EntryEnter => {
            if model.entry_enter() == tui::form::FormAction::Submit {
                model.entry_submit(port, &aliases());
            }
        }
        KeyRoute::EntryEscape => {
            if model.entry_escape() == tui::form::FormAction::ConfirmDiscard {
                model.entry_discard();
            }
        }
        KeyRoute::EntryConfirmNewSymbol => model.entry_confirm_new_symbol(port, &aliases()),
        KeyRoute::ToggleHelp => model.toggle_help(),
        KeyRoute::PickerFocusNext => model.picker_focus_next(),
        KeyRoute::PickerFocusPrev => model.picker_focus_prev(),
        KeyRoute::PickerFillFifo => model.picker_fill_fifo(),
        KeyRoute::PickerStartTake(c) => {
            if let Some(d) = c.to_digit(10) {
                model.picker_set_take_on_focus(ms(d as i64));
            }
        }
        KeyRoute::Quit | KeyRoute::Noop => {}
    }
}

/// Overtype the focused field with `s` (a real owner clearing the seeded default):
/// a printable enters text-input mode (routed `EntryStartTyping`), then backspaces
/// clear the seeded value, then the run types in.
fn overtype(model: &mut Model, port: &mut FakeRuntime, s: &str) {
    // A leading printable routes to EntryStartTyping → text-input mode (+ one char).
    dispatch(model, port, ShellKey::Char('0'));
    // Clear the whole field (the seeded value plus the sentinel char just typed).
    let len = model
        .entry_top()
        .and_then(|c| c.form.fields.get(c.form.focus))
        .map(|f| f.value.chars().count())
        .unwrap_or(0);
    for _ in 0..len {
        dispatch(model, port, ShellKey::Backspace);
    }
    for c in s.chars() {
        dispatch(model, port, ShellKey::Char(c));
    }
}

fn empty_runtime() -> FakeRuntime {
    let snap = ledger_core::Snapshot::default();
    FakeRuntime::new(
        ViewBuilder::new(snap).build(),
        flat_federal_ctx(2026, 220_000),
    )
}

// @spec TUI-VIEW-NAV-011
#[test]
fn a_dispatched_question_mark_opens_and_closes_the_overlay_without_touching_state() {
    // Drive `?` through the executed dispatch (the binary's wiring): the overlay
    // opens, swallows a would-be refresh, and closes — with the Model otherwise
    // untouched (chrome only, no state mutation).
    let mut port = empty_runtime();
    let mut model = Model::new();
    let before = model.clone();
    dispatch(&mut model, &mut port, ShellKey::Char('?'));
    assert!(model.help_open, "? opened the overlay");
    let refreshes = port.refresh_count;
    dispatch(&mut model, &mut port, ShellKey::Char('r'));
    assert_eq!(
        port.refresh_count, refreshes,
        "the modal swallowed the refresh key"
    );
    dispatch(&mut model, &mut port, ShellKey::Esc);
    assert!(!model.help_open, "esc closed the overlay");
    assert_eq!(model, before, "the round trip mutated nothing");
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn a_key_sequence_from_the_landing_screen_opens_a_buy_composer() {
    // The missing half of the keymap: from the landing Views screen, `tab` enters the
    // idle Entry panel and `b` OPENS a Buy composer (seeded through the port + config).
    // (TUI-ENTRY-FLOW-008)
    let mut port = empty_runtime();
    let mut model = Model::new();
    assert_eq!(model.mode, Mode::Views);

    // `tab` → Entry mode (idle panel, no composer yet).
    assert_eq!(route(&model, ShellKey::Tab), KeyRoute::ToggleMode);
    dispatch(&mut model, &mut port, ShellKey::Tab);
    assert_eq!(model.mode, Mode::Entry);
    assert!(
        model.entry_top().is_none(),
        "no composer is open yet — the idle panel"
    );

    // `b` on the idle panel routes to OpenFlow(Buy) and opens the composer.
    assert_eq!(
        route(&model, ShellKey::Char('b')),
        KeyRoute::OpenFlow(LaunchFlow::Buy)
    );
    dispatch(&mut model, &mut port, ShellKey::Char('b'));
    let ctx = model.entry_top().expect("a Buy composer is now open");
    assert_eq!(ctx.form.kind, FlowKind::Buy, "the open composer is a Buy");
    assert!(
        ctx.composer.is_some(),
        "the composer carries a typed Buy form for submit"
    );
    // The composer now owns the keymap: a printable starts typing the symbol field.
    assert_eq!(
        route(&model, ShellKey::Char('A')),
        KeyRoute::EntryStartTyping('A')
    );
}

// @spec TUI-ENTRY-FLOW-008
#[test]
fn the_idle_entry_panel_launches_each_flow_kind() {
    let mut port = empty_runtime();
    for (key, kind) in [
        (ShellKey::Char('b'), FlowKind::Buy),
        (ShellKey::Char('v'), FlowKind::Vest),
        (ShellKey::Char('s'), FlowKind::Sell),
        (ShellKey::Char('x'), FlowKind::Split),
    ] {
        let mut model = Model::new();
        model.toggle_mode(); // idle Entry panel
        dispatch(&mut model, &mut port, key);
        assert_eq!(
            model.entry_top().expect("a composer opened").form.kind,
            kind,
            "{key:?} opens the {kind:?} composer"
        );
    }
}

// @spec TUI-ENTRY-FLOW-003, TUI-ENTRY-ACT-006, TUI-ENTRY-FLOW-008
#[test]
fn a_full_key_run_opens_buy_edits_fields_and_submits_confirmed() {
    // Drive the WHOLE run through the routed keys: tab → b (open) → type symbol →
    // enter (advance) → type qty → … → enter (submit) → [c] confirm new symbol →
    // confirmed durable. (TUI-ENTRY-FLOW-003/008, ACT-006)
    let mut port = empty_runtime();
    let mut model = Model::new();
    dispatch(&mut model, &mut port, ShellKey::Tab); // Entry
    dispatch(&mut model, &mut port, ShellKey::Char('b')); // open Buy

    // Symbol field (index 0): overtype "AMZN".
    overtype(&mut model, &mut port, "AMZN");
    // enter commits the field + advances to Qty.
    dispatch(&mut model, &mut port, ShellKey::Enter);
    overtype(&mut model, &mut port, "10");
    dispatch(&mut model, &mut port, ShellKey::Enter); // → Unit price
    overtype(&mut model, &mut port, "100");
    // Advance through the remaining fields (Date, Fees, Platform, Tracking) with enter,
    // then enter on the last field submits.
    for _ in 0..5 {
        dispatch(&mut model, &mut port, ShellKey::Enter);
    }
    // The first submit holds on the new-symbol guard (AMZN unknown on the empty snap).
    assert!(
        model.entry_phase_confirm_new_symbol(),
        "the new-symbol guard holds the submit"
    );
    assert_eq!(
        route(&model, ShellKey::Char('c')),
        KeyRoute::EntryConfirmNewSymbol
    );
    dispatch(&mut model, &mut port, ShellKey::Char('c')); // confirm + re-submit

    assert!(
        model.entry_top().is_none(),
        "the confirmed submit cleared the composer"
    );
    assert_eq!(
        model.mode,
        Mode::Views,
        "the empty entry stack fell back to Views"
    );
    assert_eq!(
        port.ledger_log.len(),
        1,
        "the Buy landed durably through the routed submit"
    );
}

// @spec TUI-ENTRY-LOT-001, TUI-ENTRY-LOT-002, TUI-ENTRY-LOT-006, TUI-ENTRY-FLOW-008
#[test]
fn the_picker_keymap_routes_focus_take_and_fifo_fill() {
    // A Sell's open picker owns the navigation keys: `↑/↓` move the row caret, a digit
    // sets a `take` on the active row, `[F]` FIFO-fills. Routed and executed end to
    // end — the signature interaction is now operable, not render-only.
    // (TUI-ENTRY-LOT-001/002/006)
    let log = vec![
        buy(1, 15_000, "old", "AMZN", 100, 800, "Robinhood"),
        buy(2, 19_000, "new", "AMZN", 100, 17_200, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126)]);
    let mut port = FakeRuntime::new(
        ViewBuilder::new(snap.clone()).build(),
        flat_federal_ctx(2026, 220_000),
    );
    let mut model = Model::new();
    model.toggle_mode();
    // Open the Sell with a sale qty of 150 so the picker has a target (the qty field
    // edit is a separate form concern; here we drive the picker keys directly).
    let sell = tui::entry::SellForm {
        sale_id: "S1".to_string(),
        symbol: "AMZN".to_string(),
        qty: ms(150),
        unit_price: Cents(26_000),
        fees: Cents(0),
        date: Date(20_000),
        platform: "Robinhood".to_string(),
        tracking_code: None,
        accrues_to_state: Some("DC".to_string()),
        picker: tui::entry::LotPicker::build(
            &snap,
            &"AMZN".to_string(),
            "Robinhood",
            ms(150),
            Date(20_000),
        ),
    };
    model.open_composer(tui::entry::Composer::Sell(sell));
    assert!(
        model.entry_has_picker(),
        "the Sell context has an operable picker"
    );

    // The picker owns ↓ (routes to PickerFocusNext, not the form's field nav).
    assert_eq!(route(&model, ShellKey::Down), KeyRoute::PickerFocusNext);
    dispatch(&mut model, &mut port, ShellKey::Down);
    assert_eq!(model.entry_top().unwrap().picker_focus, 1);
    dispatch(&mut model, &mut port, ShellKey::Up);
    assert_eq!(model.entry_top().unwrap().picker_focus, 0);

    // A digit on the focused row routes to PickerStartTake + sets the take.
    assert_eq!(
        route(&model, ShellKey::Char('5')),
        KeyRoute::PickerStartTake('5')
    );
    dispatch(&mut model, &mut port, ShellKey::Char('5'));
    let old_take = model
        .entry_top()
        .unwrap()
        .picker_ref()
        .unwrap()
        .lots
        .iter()
        .find(|l| l.lot_id == "old")
        .unwrap()
        .take;
    assert_eq!(
        old_take,
        ms(5),
        "a routed digit set a take on the active row"
    );

    // `[F]` routes to FIFO-fill and allocates oldest-first to the sale qty.
    assert_eq!(route(&model, ShellKey::Char('F')), KeyRoute::PickerFillFifo);
    dispatch(&mut model, &mut port, ShellKey::Char('F'));
    let picker = model.entry_top().unwrap().picker_ref().unwrap();
    assert_eq!(picker.allocated(), ms(150), "FIFO filled to the sale qty");
    assert!(
        picker
            .lots
            .iter()
            .find(|l| l.lot_id == "old")
            .unwrap()
            .take
            .0
            >= ms(100).0,
        "the oldest lot fills first"
    );
}

// @spec TUI-ENTRY-FLOW-002, TUI-ENTRY-FLOW-008
#[test]
fn a_routed_submit_disagreement_rerenders_inline_and_keeps_the_composer_open() {
    // The routed submit folds a live kernel disagreement back into the inline slot —
    // the composer stays open (not cleared). (TUI-ENTRY-FLOW-002)
    let snap = replay(
        &[buy(1, 18_000, "L0", "AMZN", 5, 10_000, "Robinhood")],
        &[("AMZN", 26_126)],
    );
    let mut port = FakeRuntime::new(
        ViewBuilder::new(snap).build(),
        flat_federal_ctx(2026, 220_000),
    )
    .with_behavior(tui::testkit::SubmitBehavior::SubmitRejectLedger(
        ledger_core::LedgerError::InsufficientShares,
    ));
    let mut model = Model::new();
    dispatch(&mut model, &mut port, ShellKey::Tab);
    dispatch(&mut model, &mut port, ShellKey::Char('b'));
    overtype(&mut model, &mut port, "AMZN"); // known symbol → no new-symbol guard
    dispatch(&mut model, &mut port, ShellKey::Enter);
    overtype(&mut model, &mut port, "10");
    dispatch(&mut model, &mut port, ShellKey::Enter);
    overtype(&mut model, &mut port, "100");
    for _ in 0..5 {
        dispatch(&mut model, &mut port, ShellKey::Enter);
    }
    let ctx = model
        .entry_top()
        .expect("the composer stays open on a submit disagreement");
    match &ctx.form.phase {
        Phase::Rejected(e) => assert!(e.text().contains("InsufficientShares")),
        other => panic!("expected an inline kernel rejection, got {other:?}"),
    }
    assert!(
        ctx.form.error.is_some(),
        "the kernel error is pinned in the inline slot beside a field"
    );
}
