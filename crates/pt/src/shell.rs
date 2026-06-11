//! The TUI shell's **key-routing decision** — the pure, testable half of the binary's
//! crossterm event loop (tui-design.md → "The active context owns the keymap";
//! TUI-ENTRY-FLOW-008). The binary translates a `crossterm` `KeyEvent` into a
//! [`ShellKey`], asks [`route`] which [`KeyRoute`] it maps to given the current
//! [`tui::Model`] state, then EXECUTES that route against the Model + the live port.
//!
//! Routing is the shell's job (the children own *which keys they bind*; the shell owns
//! *how a key reaches the active context*). The decision is pure — it reads only the
//! Model's mode / open-composer / text-input state — so it is unit-tested with no TTY,
//! while the Model update methods it routes to (`entry_focus_next`, `entry_enter`,
//! `type_char`, …) are themselves tested in `tui`. (TUI-ENTRY-FLOW-008)

use std::time::Duration;

use tui::Model;

/// How long since the last **successful** refresh before the shell triggers the
/// ambient automatic one — "every hour or so is plenty". (TUI-VIEW-NAV-017)
pub const AUTOREFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The shell's pure **ambient auto-refresh decision** (TUI-VIEW-NAV-017/018): given
/// how long has elapsed since the last successful refresh (manual, automatic, or the
/// post-submit republish; the connect cycle seeds the clock) and whether an entry
/// context is open, should the event loop trigger the same refresh as `[r]` now?
///
/// The cadence is the shell's — the Model reads no clocks; the binary's loop polls on
/// a short tick, measures the elapsed time itself, and passes it here as data. An open
/// entry context suppresses the trigger (a mid-composition reflow would yank
/// fields/focus; the deferred refresh runs on the first tick after the composer
/// closes). Elapsed time and the entry stack are the ONLY inputs: the help overlay
/// does not suppress an auto-refresh (chrome over the screen it covers). A failed
/// attempt never advances the loop's last-successful clock, so this stays `true` on
/// the next tick — the retry path. (TUI-VIEW-NAV-019)
// @spec TUI-VIEW-NAV-017, TUI-VIEW-NAV-018
pub fn should_autorefresh(elapsed: Duration, entry_open: bool) -> bool {
    !entry_open && elapsed >= AUTOREFRESH_INTERVAL
}

/// An abstracted key the shell routes — the subset the keymap distinguishes, so the
/// routing decision needs no `crossterm` dependency (the binary maps `KeyEvent` →
/// `ShellKey`). (TUI-ENTRY-FLOW-008)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKey {
    Char(char),
    Tab,
    BackTab,
    Up,
    Down,
    Enter,
    Esc,
    Backspace,
    /// Any key the keymap does not bind (routes to [`KeyRoute::Noop`] / falls through).
    Other,
}

/// What the shell should do with a routed key — the side effect the binary executes
/// against the Model + port. Pure data, so the routing decision is testable apart from
/// the effect. (tui-design.md → "Errors and submits ride the Model"; TUI-ENTRY-FLOW-008)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyRoute {
    // --- Global shell bindings (Views, or an Entry key that fell through) ----------
    /// `q`/`esc` at the landing frame: quit the app.
    Quit,
    /// `esc` on a drilled frame: ascend (pop the frame).
    Ascend,
    /// `tab`: toggle the top-level Entry⇄Views mode.
    ToggleMode,
    /// `r`: re-read marks + re-replay (the views refresh).
    Refresh,
    /// `?` outside text-input mode (or `esc`/`?` while the overlay is up): toggle
    /// the modal help overlay — chrome only. (TUI-VIEW-NAV-011)
    ToggleHelp,
    /// `1`–`5` in Views mode (any frame): switch directly to a screen — the binary
    /// calls `Model::switch_screen`, which replaces the stack with the target's
    /// landing frame and retains each screen's own filter/sort/grouping.
    /// (TUI-VIEW-NAV-014)
    SwitchScreen(tui::views::Screen),

    // --- Flow launch (the idle Entry panel; TUI-ENTRY-FLOW-008) --------------------
    /// A single-key bind on the idle Entry panel (no open composer) opening a flow —
    /// `b` Buy, `v` Vest, `s` Sell, `x` Split. The binary seeds the typed composer
    /// through the port + `config` and pushes it. (TUI-ENTRY-FLOW-008)
    OpenFlow(LaunchFlow),

    // --- Entry composer keymap (the active context owns the keymap) ----------------
    /// `tab`/`↓` in field-navigation mode: move focus to the next field.
    EntryFocusNext,
    /// `shift-tab`/`↑` in field-navigation mode: move focus to the previous field.
    EntryFocusPrev,
    /// A printable key in field-navigation mode: enter text-input on the focused field
    /// and type the character (the owner just starts typing).
    EntryStartTyping(char),
    /// A printable key while in text-input mode: type the character into the field.
    EntryType(char),
    /// Backspace while in text-input mode.
    EntryBackspace,
    /// `enter`: commit on the last field (submit) or advance to the next field — the
    /// Model's `entry_enter` decides which; the binary runs the returned `FormAction`.
    EntryEnter,
    /// `esc`: in text-input mode leave the field; in field-navigation mode cancel the
    /// flow and pop the context (confirm-discard when dirty) — `entry_escape` decides.
    EntryEscape,
    /// `[c]` on a held new-symbol guard: confirm the new position + re-submit.
    /// (TUI-ENTRY-ACT-006)
    EntryConfirmNewSymbol,

    // --- Lot-picker keymap (a Sell's open picker; TUI-ENTRY-LOT-001/002/003/006) ----
    /// `↓` (field-navigation mode) with a picker open: move the picker focus caret
    /// down a row. (TUI-ENTRY-LOT-006)
    PickerFocusNext,
    /// `↑` with a picker open: move the picker focus caret up a row. (TUI-ENTRY-LOT-006)
    PickerFocusPrev,
    /// `[F]` with a picker open: FIFO-fill the allocation. (TUI-ENTRY-LOT-001)
    PickerFillFifo,
    /// A printable key starting a `take [N]` edit on the active picker row — the
    /// binary enters text-input on a synthetic take buffer. (TUI-ENTRY-LOT-002)
    PickerStartTake(char),

    /// A key the keymap does not bind in the active context (and that did not fall
    /// through to a global): do nothing.
    Noop,
}

/// Which flow the idle-Entry-panel launch keys open. The binary maps each to the
/// matching `Model::open_*` constructor (seeded through the port + `config`).
/// (TUI-ENTRY-FLOW-008)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LaunchFlow {
    Buy,
    Vest,
    Sell,
    Split,
}

/// Route a key to its [`KeyRoute`] given the Model state (TUI-ENTRY-FLOW-008). The
/// **active context owns the keymap**: when an Entry composer is open (Entry mode), the
/// key goes to the Entry keymap; a key it does not bind — and EVERY key in Views — falls
/// through to the shell's global bindings. While a field is in **text-input mode** the
/// global single-key accelerators (`tab`/`r`/printables) are suppressed so a typed
/// character is never intercepted as a command; only the field's own edit/`enter`/`esc`
/// apply. (tui-design.md → "Modality"; TUI-ENTRY-FLOW-008)
// @spec TUI-ENTRY-FLOW-008, TUI-VIEW-NAV-011, TUI-VIEW-NAV-014
pub fn route(model: &Model, key: ShellKey) -> KeyRoute {
    // The modal help overlay owns the keymap while it is up: `esc`/`?` dismiss it,
    // everything else is swallowed (a modal never leaks keys into the screen
    // beneath). (TUI-VIEW-NAV-011)
    if model.help_open {
        return match key {
            ShellKey::Esc | ShellKey::Char('?') => KeyRoute::ToggleHelp,
            _ => KeyRoute::Noop,
        };
    }
    if model.mode == tui::Mode::Entry && model.entry_top().is_some() {
        return route_entry(model, key);
    }
    route_global(model, key)
}

/// The Entry-composer keymap. (TUI-ENTRY-FLOW-008) When the active context carries a
/// **lot picker** (a Sell), the picker owns the navigation keys (the picker, not the
/// form fields, is what renders) — `↑`/`↓` move the row caret, `[F]` FIFO-fills, a
/// printable starts a `take [N]` edit — while `enter` still submits and `esc` still
/// cancels. (TUI-ENTRY-LOT-001/002/003/006)
fn route_entry(model: &Model, key: ShellKey) -> KeyRoute {
    if model.entry_in_text_input() {
        // Text-input mode: the field owns the keystroke; global accelerators suppressed.
        return match key {
            ShellKey::Esc => KeyRoute::EntryEscape,
            ShellKey::Enter => KeyRoute::EntryEnter,
            ShellKey::Backspace => KeyRoute::EntryBackspace,
            ShellKey::Char(c) => KeyRoute::EntryType(c),
            _ => KeyRoute::Noop,
        };
    }

    // `?` outside text-input mode opens the help overlay — bound ahead of the
    // start-typing printable so help is reachable from a composer; a literal `?`
    // is still typeable once a field is in text-input mode. (TUI-VIEW-NAV-011)
    if let ShellKey::Char('?') = key {
        return KeyRoute::ToggleHelp;
    }

    // A held new-symbol guard binds `[c]` to confirm-new-position. (TUI-ENTRY-ACT-006)
    if model.entry_phase_confirm_new_symbol() {
        if let ShellKey::Char('c') | ShellKey::Char('C') = key {
            return KeyRoute::EntryConfirmNewSymbol;
        }
    }

    // A Sell's open lot picker owns the navigation keys. (TUI-ENTRY-LOT-006)
    if model.entry_has_picker() {
        return match key {
            ShellKey::Down => KeyRoute::PickerFocusNext,
            ShellKey::Up => KeyRoute::PickerFocusPrev,
            ShellKey::Char('F') => KeyRoute::PickerFillFifo,
            ShellKey::Enter => KeyRoute::EntryEnter,
            ShellKey::Esc => KeyRoute::EntryEscape,
            // A digit starts a `take [N]` edit on the active row.
            ShellKey::Char(c) if c.is_ascii_digit() => KeyRoute::PickerStartTake(c),
            // `tab`/`shift-tab` still move the underlying form's field focus.
            ShellKey::Tab => KeyRoute::EntryFocusNext,
            ShellKey::BackTab => KeyRoute::EntryFocusPrev,
            _ => KeyRoute::Noop,
        };
    }

    // Field-navigation mode (a plain composer form).
    match key {
        ShellKey::Tab | ShellKey::Down => KeyRoute::EntryFocusNext,
        ShellKey::BackTab | ShellKey::Up => KeyRoute::EntryFocusPrev,
        ShellKey::Enter => KeyRoute::EntryEnter,
        ShellKey::Esc => KeyRoute::EntryEscape,
        // A printable key starts editing the focused field.
        ShellKey::Char(c) => KeyRoute::EntryStartTyping(c),
        _ => KeyRoute::Noop,
    }
}

/// The shell's global keymap (Views, or an Entry key that fell through). (tui-design.md
/// → "App Shell")
///
/// In **Entry mode with no open composer** (the idle Entry panel) the single-key
/// launch binds open a flow — `b` Buy, `v` Vest, `s` Sell, `x` Split — the missing
/// half of the keymap that gets a composer open. `tab` (back to Views) and `r`
/// (refresh) still apply; an unbound key is a no-op. (TUI-ENTRY-FLOW-008)
fn route_global(model: &Model, key: ShellKey) -> KeyRoute {
    let at_landing = model.stack.len() == 1;
    // The idle Entry panel: the launch keys open a flow.
    if model.mode == tui::Mode::Entry && model.entry.is_empty() {
        match key {
            ShellKey::Char('b') => return KeyRoute::OpenFlow(LaunchFlow::Buy),
            ShellKey::Char('v') => return KeyRoute::OpenFlow(LaunchFlow::Vest),
            ShellKey::Char('s') => return KeyRoute::OpenFlow(LaunchFlow::Sell),
            ShellKey::Char('x') => return KeyRoute::OpenFlow(LaunchFlow::Split),
            ShellKey::Tab => return KeyRoute::ToggleMode,
            ShellKey::Char('r') => return KeyRoute::Refresh,
            ShellKey::Char('?') => return KeyRoute::ToggleHelp, // (TUI-VIEW-NAV-011)
            ShellKey::Esc => return KeyRoute::ToggleMode, // esc leaves the idle Entry panel back to Views
            _ => return KeyRoute::Noop,
        }
    }
    match key {
        // `q` quits only at the landing frame and with no open composer.
        ShellKey::Char('q') if at_landing && model.entry.is_empty() => KeyRoute::Quit,
        ShellKey::Esc if at_landing => KeyRoute::Quit,
        ShellKey::Esc => KeyRoute::Ascend,
        ShellKey::Tab => KeyRoute::ToggleMode,
        ShellKey::Char('r') => KeyRoute::Refresh,
        ShellKey::Char('?') => KeyRoute::ToggleHelp, // (TUI-VIEW-NAV-011)
        // `1`–`5` switch directly between the Views screens, from any Views frame
        // (drilled or not). (TUI-VIEW-NAV-014)
        ShellKey::Char('1') => KeyRoute::SwitchScreen(tui::views::Screen::Positions),
        ShellKey::Char('2') => KeyRoute::SwitchScreen(tui::views::Screen::OpenLots),
        ShellKey::Char('3') => KeyRoute::SwitchScreen(tui::views::Screen::History),
        ShellKey::Char('4') => KeyRoute::SwitchScreen(tui::views::Screen::TaxReserves),
        ShellKey::Char('5') => KeyRoute::SwitchScreen(tui::views::Screen::Realized),
        _ => KeyRoute::Noop,
    }
}
