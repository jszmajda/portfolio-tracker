//! Status-line key hints + the modal help overlay (views-design.md →
//! "Status-line key hints" / "Help Overlay"). The hints follow the active
//! context and are suppressed in text-input mode; `?` opens a centered modal
//! panel with the full keymap grouped by context — chrome only.

mod common;

use tui::entry::BuyForm;
use tui::form::FormModel;
use tui::testkit::{render_string, FakeRuntime};
use tui::{key_hints, EntryContext, Model};

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

fn runtime() -> FakeRuntime {
    common::two_position_runtime()
}

// @spec TUI-VIEW-NAV-010, TUI-VIEW-NAV-015
#[test]
fn key_hints_follow_the_active_context() {
    // Views, landing frame: tab/1-5/r/?/q.
    let mut model = Model::new();
    let hints = key_hints(&model);
    assert!(hints.contains(&("tab", "entry")));
    assert!(hints.contains(&("1-5", "screens")), "the between-screens hint rides every Views frame");
    assert!(hints.contains(&("r", "refresh")));
    assert!(hints.contains(&("?", "help")));
    assert!(hints.contains(&("q", "quit")), "the landing frame hints quit");
    assert!(!hints.contains(&("esc", "back")));

    // A drilled frame hints esc-back instead of quit — and still the screens hint.
    assert!(model.drill(&tui::views::RowIdentity::Symbol("AMZN".to_string())));
    let drilled = key_hints(&model);
    assert!(drilled.contains(&("esc", "back")), "a drilled frame hints back");
    assert!(drilled.contains(&("1-5", "screens")), "screens are reachable from a drilled frame");
    assert!(!drilled.contains(&("q", "quit")), "q does not quit off the landing frame");
    model.ascend();

    // The idle Entry panel hints its flow-launch keys.
    model.toggle_mode();
    let idle = key_hints(&model);
    for (k, w) in [("b", "buy"), ("v", "vest"), ("s", "sell"), ("x", "split")] {
        assert!(idle.contains(&(k, w)), "the idle Entry panel hints [{k}] {w}");
    }

    // An open composer hints its field keys.
    model.open_entry(EntryContext::form(FormModel::for_buy(&a_buy())));
    let composing = key_hints(&model);
    assert!(composing.contains(&("enter", "submit")));
    assert!(composing.contains(&("esc", "cancel")));
    assert!(!composing.contains(&("b", "buy")), "launch keys are not live in a composer");
}

// @spec TUI-VIEW-NAV-010
#[test]
fn key_hints_are_suppressed_in_text_input_mode_and_render_on_the_status_line() {
    let rt = runtime();
    let model = Model::new();
    // The hints ride the status line's right edge in the rendered buffer.
    let s = render_string(&model, &rt, 140, 24);
    let bottom = s.lines().last().unwrap();
    assert!(bottom.contains("[tab] entry"), "the hints render: {bottom}");
    assert!(bottom.contains("[1-5] screens"), "the screens hint renders: {bottom}");
    assert!(bottom.contains("[?] help"), "the help hint renders: {bottom}");
    assert!(bottom.contains("[q] quit"), "the quit hint renders: {bottom}");

    // The bracketed key renders in the accent role beside fg-faint words.
    let buf = tui::render_to_buffer(&model, &rt, 140, 24);
    let cols: Vec<String> = (0..140u16).map(|x| buf[(x, 23)].symbol().to_string()).collect();
    let bracket_x = (0..cols.len() - 4)
        .find(|&x| cols[x..x + 5].concat() == "[tab]")
        .expect("the [tab] hint on the bottom row") as u16;
    let accent = tui::theme::Palette::ledger().color(tui::theme::Role::Accent);
    assert_eq!(
        buf[(bracket_x, 23)].style().fg,
        Some(accent),
        "the bracketed key renders accent"
    );

    // Text-input mode suppresses the hints entirely (the field owns every key).
    let mut model = Model::new();
    model.open_entry(EntryContext::form(FormModel::for_buy(&a_buy())));
    model.entry_enter_field();
    assert!(model.entry_in_text_input());
    assert!(key_hints(&model).is_empty(), "text-input mode suppresses the hints");
    let s = render_string(&model, &rt, 140, 24);
    let bottom = s.lines().last().unwrap();
    assert!(!bottom.contains("[?] help"), "no hints while typing: {bottom}");
}

// @spec TUI-VIEW-NAV-011
#[test]
fn the_help_overlay_opens_closes_and_mutates_nothing_else() {
    let mut model = Model::new();
    let before = model.clone();
    model.toggle_help();
    assert!(model.help_open, "? opens the overlay");
    // Chrome only: nothing but the flag moved.
    assert_eq!(model.mode, before.mode);
    assert_eq!(model.stack, before.stack);
    assert_eq!(model.entry, before.entry);
    model.toggle_help();
    assert!(!model.help_open, "a second toggle closes it");
    assert_eq!(model, before, "the round trip leaves the Model untouched");
}

// @spec TUI-VIEW-NAV-011, TUI-VIEW-NAV-015
#[test]
fn the_help_overlay_renders_the_grouped_keymap_over_the_current_screen() {
    let rt = runtime();
    let mut model = Model::new();
    model.toggle_help();
    let s = render_string(&model, &rt, 110, 42);
    // The full keymap, grouped by context — the screens group opens the overlay
    // and the picker group names its host flow. (TUI-VIEW-NAV-015)
    for group in ["SCREENS", "GLOBAL", "VIEWS", "ENTRY", "LOT PICKER (INSIDE A SELL)"] {
        assert!(s.contains(group), "the {group} group renders: {s}");
    }
    // Two-column key/action listing.
    assert!(s.contains("[tab]"), "keys render bracketed");
    assert!(s.contains("FIFO-fill the allocation"), "picker actions render");
    assert!(s.contains("switch entry"), "global actions render");
    // It rides OVER the current screen — the chrome around it survives.
    assert!(s.contains("L E D G E R"), "the masthead stays beneath the overlay");
    assert!(s.to_uppercase().contains("HELP"), "the gilt panel title names Help");
}

// @spec TUI-VIEW-NAV-015
#[test]
fn the_help_overlay_opens_with_a_screens_section_naming_each_screen_and_its_key() {
    let groups = tui::help_keymap();
    let (first_name, screens) = &groups[0];
    assert_eq!(*first_name, "screens", "the screens section opens the overlay");
    let expect = [
        ("1", "Positions"),
        ("2", "Open Lots"),
        ("3", "History"),
        ("4", "Tax & Reserves"),
        ("5", "Realized"),
    ];
    for (key, screen) in expect {
        assert!(
            screens.contains(&(key, screen)),
            "the screens section names {screen} beside [{key}]: {screens:?}"
        );
    }
    // The picker group names its host flow so the context is identifiable.
    assert!(
        groups.iter().any(|(name, _)| *name == "lot picker (inside a Sell)"),
        "the lot-picker group names the Sell that hosts it"
    );
    // The rendered screens rows pair key and screen name.
    let rt = runtime();
    let mut model = Model::new();
    model.toggle_help();
    let s = render_string(&model, &rt, 110, 42);
    assert!(s.contains("[4]"), "the screen key renders bracketed: {s}");
    assert!(s.contains("Tax & Reserves"), "the screen name renders: {s}");
}
